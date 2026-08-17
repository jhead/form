//! Port of `.upstream/packages/server/src/types.ts`.
//!
//! [`SessionService`] (upstream `PiServerService`) and [`SessionRuntime`]
//! (upstream `PiSessionRuntime`) are the server's only dependency on the agent
//! and session crates: this crate owns the port and the concrete
//! implementation lives outside it.

use std::sync::Arc;

use async_trait::async_trait;
use pi_protocol::{
    ModelMetadata, ModelRef, SessionMetadata, SessionPhase, SessionSnapshot, ThinkingLevel,
    TranscriptProgress,
};

use crate::errors::{PiServerError, TransportError};
use crate::listener::PiServerListener;

/// Callback handle returned by [`SessionRuntime::subscribe`].
#[derive(Clone)]
pub struct Unsubscribe(Arc<dyn Fn() + Send + Sync>);

impl Unsubscribe {
    pub fn new(action: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(action))
    }

    pub fn noop() -> Self {
        Self::new(|| {})
    }

    pub fn unsubscribe(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for Unsubscribe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Unsubscribe")
    }
}

/// `Omit<PromptCommand, "command" | "sessionId">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptInput {
    pub text: String,
}

/// Upstream's `SteerInput`, structurally identical to [`PromptInput`].
pub type SteerInput = PromptInput;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateSessionOptions {
    /// A collision-resistant ID assigned by `PiServer`. The service must
    /// persist this exact ID.
    pub id: String,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// `Progress` is both the largest and by far the most frequent variant (one per
/// streaming delta), so boxing it would put an allocation on the hot path for
/// the benefit of the rare ones.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum SessionRuntimeEvent {
    Snapshot,
    Progress(TranscriptProgress),
    /// Terminal: the server disconnects every attached client and disposes the
    /// runtime.
    Error(PiServerError),
}

pub type RuntimeEventListener = Arc<dyn Fn(SessionRuntimeEvent) + Send + Sync>;

/// One acquired durable session. Conflicting operations must fail rather than
/// queue.
#[async_trait]
pub trait SessionRuntime: Send + Sync + 'static {
    async fn snapshot(&self) -> Result<SessionSnapshot, PiServerError>;
    fn phase(&self) -> SessionPhase;
    async fn prompt(&self, input: PromptInput) -> Result<(), PiServerError>;
    async fn steer(&self, input: SteerInput) -> Result<(), PiServerError>;
    async fn abort(&self) -> Result<(), PiServerError>;
    async fn set_model(&self, model: ModelRef) -> Result<(), PiServerError>;
    async fn set_thinking(&self, thinking_level: ThinkingLevel) -> Result<(), PiServerError>;
    fn subscribe(&self, listener: RuntimeEventListener) -> Unsubscribe;
    async fn dispose(&self) -> Result<(), PiServerError>;
}

/// Service boundary for durable sessions and exclusively acquired runtimes.
///
/// Upstream calls this `PiServerService`.
#[async_trait]
pub trait SessionService: Send + Sync + 'static {
    async fn list_sessions(&self) -> Result<Vec<SessionMetadata>, PiServerError>;
    async fn list_models(&self) -> Result<Vec<ModelMetadata>, PiServerError>;
    async fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<Arc<dyn SessionRuntime>, PiServerError>;
    async fn open_session(
        &self,
        session_id: String,
    ) -> Result<Arc<dyn SessionRuntime>, PiServerError>;
}

pub type ServerErrorHandler = Arc<dyn Fn(ServerErrorReport) + Send + Sync>;

/// What `on_error` observers receive. Upstream passes an `Error`; this keeps
/// the classification so a consumer can tell a transport failure from a
/// service failure without string matching.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ServerErrorReport {
    #[error("{0}")]
    Service(PiServerError),
    #[error("{0}")]
    Transport(TransportError),
    #[error("{0}")]
    Protocol(pi_protocol::ProtocolValidationError),
    #[error("{0}")]
    Other(String),
}

impl ServerErrorReport {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Service(error) => error.code(),
            Self::Transport(_) => "transport",
            Self::Protocol(error) => error.code(),
            Self::Other(_) => "other",
        }
    }
}

#[derive(Clone, Default)]
pub struct PiServerOptions {
    pub listeners: Vec<Arc<dyn PiServerListener>>,
    pub max_frame_length: Option<u32>,
    pub handshake_timeout_ms: Option<u64>,
    pub server_id: Option<String>,
    pub on_error: Option<ServerErrorHandler>,
}

impl PiServerOptions {
    pub fn new(listeners: Vec<Arc<dyn PiServerListener>>) -> Self {
        Self {
            listeners,
            ..Self::default()
        }
    }

    pub fn with_max_frame_length(mut self, max_frame_length: u32) -> Self {
        self.max_frame_length = Some(max_frame_length);
        self
    }

    pub fn with_handshake_timeout_ms(mut self, handshake_timeout_ms: u64) -> Self {
        self.handshake_timeout_ms = Some(handshake_timeout_ms);
        self
    }

    pub fn with_server_id(mut self, server_id: impl Into<String>) -> Self {
        self.server_id = Some(server_id.into());
        self
    }

    pub fn with_error_handler(mut self, on_error: ServerErrorHandler) -> Self {
        self.on_error = Some(on_error);
        self
    }
}
