//! `google-generative-ai` and `google-vertex` API adapters.
//!
//! Port of `packages/ai/src/api/{google-generative-ai,google-shared,
//! google-vertex}.ts` plus the two provider descriptors in
//! `packages/ai/src/providers/`.
//!
//! # Differences from upstream, and why
//!
//! - **No SDK.** Upstream calls `@google/genai`; this crate speaks the same
//!   HTTP through `pi-http`. [`wire`] documents the shapes, and the URL rules
//!   in [`google_vertex`] reproduce the SDK's `constructUrl` exactly.
//! - **Vertex credentials are a trait.** `google-auth-library` has no
//!   equivalent here and interactive login belongs to `pi-auth`, so the adapter
//!   takes an `Arc<dyn GoogleTokenSource>`. See [`token_source`].
//! - **`partial` snapshots are cloned.** Upstream aliases a single mutable
//!   `output` object into every event; the Rust port clones so each event
//!   carries a true point-in-time snapshot.
//! - **Shared adapter logic is not duplicated here.**
//!   `api/{transform-messages,simple-options,constrained-sampling}.ts` and
//!   `models.ts#calculateCost` live in [`pi_provider_common`]; the thinking-level
//!   clamp is [`pi_core::Model::clamp_thinking_level`].
//!
//! # Usage
//!
//! ```no_run
//! # async fn run() -> Result<(), pi_core::AiError> {
//! use pi_core::{ApiClient, Context, Model, StreamOptions};
//! use pi_core::message::{Message, UserMessage};
//! use pi_provider_google::GoogleGenerativeAiClient;
//!
//! let client = GoogleGenerativeAiClient::new();
//! let model = Model::new(
//!     "gemini-2.5-flash",
//!     pi_core::Api::GoogleGenerativeAi,
//!     "google",
//!     "https://generativelanguage.googleapis.com/v1beta",
//! );
//! let context = Context::new(vec![Message::User(UserMessage::text("hi"))]);
//! let mut options = StreamOptions::default();
//! options.request.api_key = Some(std::env::var("GEMINI_API_KEY").unwrap_or_default());
//!
//! let stream = client.stream(&model, &context, &options).await?;
//! let message = stream.into_final_message().await;
//! # let _ = message;
//! # Ok(())
//! # }
//! ```

pub mod google_generative_ai;
pub mod google_shared;
pub mod google_vertex;
pub mod options;
mod params;
pub mod provider;
mod stream;
pub mod token_source;
pub mod wire;

pub use google_generative_ai::GoogleGenerativeAiClient;
pub use google_shared::{
    calculate_cost, clamp_max_tokens_to_context, convert_messages, convert_tools, is_thinking_part,
    map_stop_reason, map_tool_choice, requires_tool_call_id, resolve_google_function_calling_mode,
    retain_thought_signature, supports_google_strict_tool_sampling,
};
pub use google_vertex::GoogleVertexClient;
pub use options::{GoogleOptions, GoogleStreamOptionsExt, GoogleThinking, GoogleToolChoice};
pub use provider::{
    google_generative_ai_client, google_provider_descriptor, google_provider_descriptors,
    google_vertex_client, google_vertex_provider_descriptor, ProviderAuthDescriptor,
    ProviderDescriptor,
};
pub use token_source::{
    default_token_source, AdcFileTokenSource, CachingTokenSource, ChainTokenSource, EnvTokenSource,
    GoogleAccessToken, GoogleTokenRequest, GoogleTokenSource, GoogleTokenSourceRef,
    MetadataServerTokenSource, StaticTokenSource, CLOUD_PLATFORM_SCOPE,
};
pub use wire::{GenerateContentRequest, GenerateContentResponse, GoogleThinkingLevel};
