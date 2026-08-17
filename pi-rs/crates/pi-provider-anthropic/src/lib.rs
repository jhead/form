//! `anthropic-messages` API adapter.
//!
//! Port of `packages/ai/src/api/anthropic-messages.ts` (+ `.lazy.ts`) and the
//! static half of `packages/ai/src/providers/anthropic.ts`.
//!
//! ```no_run
//! use std::sync::Arc;
//! use pi_core::{ApiClient, ApiClientRef, Context, Message, Model, UserMessage};
//! use pi_core::model::Api;
//! use pi_provider_anthropic::{AnthropicMessagesApi, ANTHROPIC_PROVIDER};
//!
//! # async fn run() -> Result<(), pi_core::AiError> {
//! let api: ApiClientRef = Arc::new(AnthropicMessagesApi::new());
//! let model = Model::new(
//!     "claude-opus-4-8",
//!     Api::AnthropicMessages,
//!     ANTHROPIC_PROVIDER.id,
//!     ANTHROPIC_PROVIDER.base_url,
//! );
//! let context = Context::new(vec![Message::User(UserMessage::text("hi"))]);
//! let mut options = pi_core::SimpleStreamOptions::default();
//! options.stream.request.api_key = Some("sk-ant-...".into());
//! let stream = api.stream_simple(&model, &context, &options).await?;
//! # Ok(())
//! # }
//! ```
//!
//! The shared upstream helpers this adapter needs live in the crates that own
//! them: `api/{transform-messages,simple-options,constrained-sampling}.ts` and
//! `models.ts#calculateCost` in [`pi_provider_common`],
//! `utils/{json-parse,estimate,provider-env}.ts` in [`pi_http`]. Only
//! `utils/deferred-tools.ts` is still local, because Anthropic's tool-reference
//! placement is the only consumer.

pub mod anthropic_messages;
pub mod deferred_tools;
pub mod options;
pub mod provider;
pub mod request;

/// The API id this adapter serves.
pub const ANTHROPIC_MESSAGES_API: &str = "anthropic-messages";

pub use anthropic_messages::AnthropicMessagesApi;
pub use options::{
    AnthropicEffort, AnthropicOptions, AnthropicThinkingDisplay, AnthropicToolChoice,
    ToolChoiceMode, ToolChoiceTool,
};
pub use provider::{ProviderDescriptor, ANTHROPIC_PROVIDER, ANTHROPIC_VERSION_HEADER};
pub use request::{
    anthropic_compat, from_claude_code_name, is_oauth_token, to_claude_code_name, AnthropicCompat,
    FINE_GRAINED_TOOL_STREAMING_BETA, INTERLEAVED_THINKING_BETA,
};
