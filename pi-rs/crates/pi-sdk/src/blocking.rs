//! Synchronous entry points for hosts without a Rust async runtime.
//!
//! A Swift caller cannot poll a Rust future, and it cannot drop one to cancel
//! it either — which is why every cancellable operation in this SDK takes an
//! explicit [`AbortSignal`] rather than relying on future cancellation. This
//! module owns a runtime and blocks on it, so the FFI layer only ever sees
//! ordinary synchronous calls.
//!
//! ```no_run
//! use pi_sdk::blocking::Runtime;
//! use pi_sdk::Pi;
//!
//! # fn main() -> Result<(), pi_sdk::SdkError> {
//! let rt = Runtime::new()?;
//! let pi = Pi::builder().with_builtin_providers().build()?;
//! let model = rt.block_on(pi.resolve_model("anthropic/claude-sonnet-4-5"))?;
//! # Ok(())
//! # }
//! ```

use std::future::Future;
use std::sync::Arc;

use pi_agent::{Agent, AgentEvent, AgentEventListener};
use pi_core::{AbortHandle, AbortSignal, ImageContent};

use crate::SdkError;

/// A multi-threaded Tokio runtime owned by the SDK.
///
/// Hold one for the lifetime of the host process. Cloning shares the same
/// runtime, so it is safe to hand copies to different FFI objects.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<tokio::runtime::Runtime>,
}

impl Runtime {
    pub fn new() -> Result<Self, SdkError> {
        let inner = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| SdkError::Config(format!("failed to start runtime: {e}")))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Run a future to completion on this runtime.
    ///
    /// # Panics
    ///
    /// Panics if called from inside a Tokio runtime — Tokio forbids nested
    /// `block_on`. From async Rust, await the future directly instead.
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.inner.block_on(future)
    }

    /// Spawn a future without waiting for it. Use with
    /// [`AbortHandle`] so the host can still cancel it.
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.inner.spawn(future);
    }

    pub fn handle(&self) -> &tokio::runtime::Runtime {
        &self.inner
    }
}

/// Adapts a plain callback into an [`AgentEventListener`].
///
/// The callback is invoked from the runtime's threads, so it must be `Send +
/// Sync`. An FFI bridge typically posts the event onto the host's own queue
/// here rather than doing work inline.
pub struct CallbackListener<F> {
    callback: F,
}

impl<F> CallbackListener<F>
where
    F: Fn(AgentEvent) + Send + Sync + 'static,
{
    pub fn new(callback: F) -> Arc<Self> {
        Arc::new(Self { callback })
    }
}

#[async_trait::async_trait]
impl<F> AgentEventListener for CallbackListener<F>
where
    F: Fn(AgentEvent) + Send + Sync + 'static,
{
    async fn on_event(&self, event: AgentEvent, _signal: AbortSignal) {
        (self.callback)(event);
    }
}

/// A prompt running in the background, with a handle to cancel it.
///
/// This is the shape an FFI host wants: start the turn, receive events through
/// a callback, and keep something to cancel with — no future to hold.
pub struct RunningPrompt {
    abort: AbortHandle,
    agent: Agent,
}

impl RunningPrompt {
    /// Cancel the turn. Idempotent.
    pub fn abort(&self) {
        self.abort.abort();
        self.agent.abort();
    }

    pub fn is_streaming(&self) -> bool {
        self.agent.is_streaming()
    }
}

/// Blocking conveniences on [`Agent`].
pub trait AgentBlockingExt {
    /// Send a prompt and block until the turn settles.
    fn prompt_text_blocking(
        &self,
        runtime: &Runtime,
        input: &str,
        images: Vec<ImageContent>,
    ) -> Result<(), SdkError>;

    /// Send a prompt, delivering events to `on_event` as they arrive, and block
    /// until the turn settles.
    fn prompt_text_streaming(
        &self,
        runtime: &Runtime,
        input: &str,
        on_event: impl Fn(AgentEvent) + Send + Sync + 'static,
    ) -> Result<(), SdkError>;

    /// Start a prompt in the background and return immediately.
    fn prompt_text_background(
        &self,
        runtime: &Runtime,
        input: &str,
        on_event: impl Fn(AgentEvent) + Send + Sync + 'static,
    ) -> RunningPrompt;
}

impl AgentBlockingExt for Agent {
    fn prompt_text_blocking(
        &self,
        runtime: &Runtime,
        input: &str,
        images: Vec<ImageContent>,
    ) -> Result<(), SdkError> {
        runtime.block_on(async { self.prompt_text(input, images).await })?;
        Ok(())
    }

    fn prompt_text_streaming(
        &self,
        runtime: &Runtime,
        input: &str,
        on_event: impl Fn(AgentEvent) + Send + Sync + 'static,
    ) -> Result<(), SdkError> {
        let subscription = self.subscribe(CallbackListener::new(on_event));
        let result = runtime.block_on(async { self.prompt_text(input, vec![]).await });
        subscription.unsubscribe();
        result?;
        Ok(())
    }

    fn prompt_text_background(
        &self,
        runtime: &Runtime,
        input: &str,
        on_event: impl Fn(AgentEvent) + Send + Sync + 'static,
    ) -> RunningPrompt {
        let (abort, _signal) = AbortHandle::new();
        self.subscribe(CallbackListener::new(on_event));

        let agent = self.clone();
        let input = input.to_string();
        runtime.spawn(async move {
            let _ = agent.prompt_text(input, vec![]).await;
        });

        RunningPrompt {
            abort,
            agent: self.clone(),
        }
    }
}
