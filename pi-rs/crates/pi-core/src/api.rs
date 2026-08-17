//! The provider/API abstraction every adapter crate implements.
//!
//! Port of `ProviderStreams` / `StreamFunction` from `packages/ai/src/types.ts`.
//!
//! Contract (unchanged from upstream):
//! - `stream` must not return `Err` for request/model/runtime failures. Those are
//!   encoded in the returned stream as an `Error` event carrying an
//!   `AssistantMessage` with `stop_reason` `Error` or `Aborted`.
//! - `Err` is reserved for programmer errors detected before any request work
//!   begins (for example an adapter that cannot serve the given model).

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::AiError;
use crate::event::AssistantMessageEventStream;
use crate::message::DeferredHandle;
use crate::model::Model;
use crate::options::{DeferredFetchOptions, RequestOptions, SimpleStreamOptions, StreamOptions};
use crate::tool::Context;

/// One API wire format (anthropic-messages, openai-completions, ...).
///
/// Object-safe on purpose: providers are stored as `Arc<dyn ApiClient>` in the
/// registry and handed across the FFI boundary as opaque handles.
#[async_trait]
pub trait ApiClient: Send + Sync + 'static {
    /// Stable API id, e.g. `"anthropic-messages"`.
    fn api(&self) -> &str;

    /// Stream a completion using raw, API-specific options.
    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError>;

    /// Stream with unified reasoning options. Adapters map `reasoning` and
    /// `thinking_budgets` onto their own knobs before delegating to `stream`.
    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError>;

    /// Whether this adapter can serve deferred responses.
    fn supports_deferred(&self) -> bool {
        false
    }

    async fn fetch_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: &DeferredFetchOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        Err(AiError::unsupported(
            "deferred responses are not supported by this API",
        ))
    }

    async fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        _options: &RequestOptions,
    ) -> Result<(), AiError> {
        Err(AiError::unsupported(
            "deferred responses are not supported by this API",
        ))
    }
}

/// Shared handle to an API adapter.
pub type ApiClientRef = Arc<dyn ApiClient>;

/// The stream entry point the agent loop depends on (`StreamFn` upstream).
///
/// Deliberately not a trait object over generics: a plain `Arc<dyn Fn>` keeps
/// the agent loop independent of the provider registry and is trivial to fake.
pub type StreamFn = Arc<
    dyn Fn(
            Model,
            Context,
            SimpleStreamOptions,
        )
            -> futures_core::future::BoxFuture<'static, Result<AssistantMessageEventStream, AiError>>
        + Send
        + Sync,
>;

/// Wrap an [`ApiClient`] as a [`StreamFn`].
pub fn stream_fn_from_client(client: ApiClientRef) -> StreamFn {
    Arc::new(move |model, context, options| {
        let client = client.clone();
        Box::pin(async move { client.stream_simple(&model, &context, &options).await })
    })
}
