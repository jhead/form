//! One-stop facade over the Pi SDK crates.
//!
//! The workspace is split into focused crates so they can be developed and
//! tested independently. Most embedders want the whole thing, so this crate
//! re-exports the public surface, wires the pieces together through
//! [`Pi::builder`], and adds the shapes an FFI host needs that idiomatic Rust
//! does not: a [`blocking`] runtime handle and [`json`] entry points.
//!
//! ```no_run
//! use pi_sdk::{Pi, ModelThinkingLevel};
//!
//! # async fn run() -> Result<(), pi_sdk::SdkError> {
//! let pi = Pi::builder().with_builtin_providers().build()?;
//! let model = pi.resolve_model("anthropic/claude-sonnet-4-5").await?;
//!
//! let agent = pi.agent().model(model).system_prompt("You are terse.").build()?;
//! agent.prompt_text("What is 2 + 2?", vec![]).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Layout
//!
//! - [`Pi`] / [`PiBuilder`] — the assembled SDK: model catalog, provider
//!   adapters, credential resolution.
//! - [`AgentBuilder`] — builds a configured [`Agent`] against that catalog.
//! - [`blocking`] — call the async API from a thread with no runtime.
//! - [`json`] — JSON-in/JSON-out wrappers for bridging.
//! - [`core`], [`agent`], [`catalog`], [`auth`], [`tools`], [`session`],
//!   [`sqlite`], [`providers`], [`telemetry`], [`http`] — the underlying crates,
//!   for anything the facade does not surface.

pub mod blocking;
pub mod builder;
pub mod json;

pub use builder::{AgentBuilder, Pi, PiBuilder, SdkError};

/// The underlying crates, re-exported under stable names.
pub mod core {
    pub use pi_core::*;
}
pub mod agent {
    pub use pi_agent::*;
}
pub mod catalog {
    pub use pi_catalog::*;
}
pub mod auth {
    pub use pi_auth::*;
}
pub mod tools {
    pub use pi_tools::*;
}
pub mod session {
    pub use pi_session::*;
}
pub mod sqlite {
    pub use pi_session_sqlite::*;
}
pub mod telemetry {
    pub use pi_telemetry::*;
}
pub mod http {
    pub use pi_http::*;
}

/// Provider adapters, one module per API family.
pub mod providers {
    pub use pi_provider_anthropic as anthropic;
    pub use pi_provider_google as google;
    pub use pi_provider_misc as misc;
    pub use pi_provider_openai as openai;

    /// Every built-in adapter, ready to register on a [`crate::catalog::ModelRegistry`].
    pub fn builtin_api_clients() -> Vec<pi_core::ApiClientRef> {
        let mut clients: Vec<pi_core::ApiClientRef> =
            vec![std::sync::Arc::new(anthropic::AnthropicMessagesApi::new())];
        clients.extend(openai::all_api_clients());
        clients.push(std::sync::Arc::new(google::GoogleGenerativeAiClient::new()));
        clients.push(std::sync::Arc::new(google::GoogleVertexClient::new()));
        clients.push(std::sync::Arc::new(misc::MistralConversationsApi::new()));
        clients.push(std::sync::Arc::new(misc::PiMessagesApi::new()));
        clients
    }
}

/// The types most embedders touch. `use pi_sdk::prelude::*;`
pub mod prelude {
    pub use crate::builder::{AgentBuilder, Pi, PiBuilder, SdkError};
    pub use pi_agent::{Agent, AgentEvent, AgentEventListener, AgentMessage, AgentState};
    pub use pi_core::{
        AbortHandle, AbortSignal, AiError, AssistantMessage, AssistantMessageEvent, Context,
        Message, Model, ModelThinkingLevel, StopReason, ThinkingLevel, Tool, ToolResultMessage,
        Usage, UserMessage,
    };
    pub use pi_tools::{AgentTool, AgentToolRef, ExecutionEnvRef, LocalExecutionEnv, ToolResult};
}

// Flat re-exports of the handful of types that appear in facade signatures, so
// `Pi::builder()` is usable without reaching into the sub-crate modules.
pub use pi_agent::{Agent, AgentEvent, AgentEventListener, AgentMessage};
pub use pi_core::{
    AbortHandle, AbortSignal, AiError, AssistantMessage, Model, ModelThinkingLevel, Tool,
};
pub use pi_tools::{AgentToolRef, ExecutionEnvRef};
