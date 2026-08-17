//! Core wire types and traits for the Pi SDK.
//!
//! This is the Rust port of the side-effect-free core of
//! `@earendil-works/pi-ai` (`packages/ai/src/types.ts` and friends). Every other
//! crate in the workspace depends on this one and nothing else in the workspace,
//! so it is the shared contract between the provider adapters, the agent runtime
//! and the session layer.
//!
//! ## Design rules
//!
//! These hold across the whole workspace, because the SDK is consumed from Swift
//! over FFI:
//!
//! - Public types are owned, `'static`, `Send + Sync`, and `serde`-serializable.
//!   JSON is the bridge format, so field names match the TypeScript wire shape
//!   (`camelCase`) exactly — sessions written by the TS implementation must load
//!   here and vice versa.
//! - No lifetimes and no generic parameters in public API signatures. Where
//!   upstream is generic over a TypeBox schema, this port carries
//!   `serde_json::Value` JSON Schema instead.
//! - Extension points are object-safe traits behind `Arc<dyn Trait>`.
//! - Errors are a single flat enum with a stable `code()` string.

pub mod api;
pub mod content;
pub mod error;
pub mod event;
pub mod message;
pub mod model;
pub mod options;
pub mod tool;
pub mod uuid;

pub use api::{stream_fn_from_client, ApiClient, ApiClientRef, StreamFn};
pub use content::{
    AssistantContent, ImageContent, InputContent, TextContent, TextPhase, TextSignatureV1,
    ThinkingContent, ToolCall,
};
pub use error::AiError;
pub use event::{
    AssistantMessageEvent, AssistantMessageEventSink, AssistantMessageEventStream, DoneReason,
    ErrorReason,
};
pub use message::{
    now_ms, AssistantMessage, AssistantMessageDiagnostic, Cost, DeferredHandle, DiagnosticSeverity,
    Message, StopReason, TimestampMs, ToolResultMessage, Usage, UserContent, UserMessage,
};
pub use model::{
    Api, CacheRetention, ImagesModel, MaxTokensField, Modality, Model, ModelCompat, ModelCost,
    ModelCostRates, ModelCostTier, ModelThinkingLevel, ProviderId, SessionAffinityFormat,
    ThinkingBudgets, ThinkingFormat, ThinkingLevel, ThinkingLevelMap, Transport,
    EXTENDED_THINKING_LEVELS,
};
pub use options::{
    AbortHandle, AbortSignal, Deferred, DeferredFetchOptions, DeferredWindow, OnPayload,
    OnResponse, ProviderEnv, ProviderHeaders, ProviderResponse, RequestOptions,
    SimpleStreamOptions, StreamOptions,
};
pub use tool::{
    ConstrainedSampling, ConstrainedSamplingConfig, Context, GrammarFormat, GrammarVariants,
    StrictMode, Tool,
};

pub use uuid::{uuidv7, uuidv7_from, UuidV7State};
