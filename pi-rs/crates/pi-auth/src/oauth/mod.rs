//! Port of `packages/ai/src/auth/oauth/`.
//!
//! Every flow implements [`OAuthFlow`](crate::provider_auth::OAuthFlow) and
//! reaches the user only through
//! [`AuthInteraction`](crate::AuthInteraction) — no stdin, no browser launch.
//! The loopback callback server in [`callback_server`] is opt-in per login.

pub mod anthropic;
pub mod callback_server;
pub mod device_code;
pub mod github_copilot;
pub mod kimi_coding;
pub mod load;
pub mod oauth_page;
pub mod openai_codex;
pub mod openrouter;
pub mod pkce;
pub mod radius;
pub mod shared;
pub mod xai;

pub use anthropic::AnthropicOAuth;
pub use callback_server::{callback_host, CallbackPage, CallbackParams, LoopbackCallback};
pub use device_code::{abortable_sleep, poll_device_code_flow, DeviceCodePollOptions, PollResult};
pub use github_copilot::GitHubCopilotOAuth;
pub use kimi_coding::KimiCodingOAuth;
pub use load::{
    load_oauth_flow, register_oauth_flow, unregister_oauth_flow, OAuthFlows,
    BUILT_IN_OAUTH_PROVIDERS,
};
pub use oauth_page::{oauth_error_html, oauth_success_html};
pub use openai_codex::OpenAICodexOAuth;
pub use openrouter::OpenRouterOAuth;
pub use pkce::{generate_pkce, pkce_challenge, Pkce};
pub use radius::{normalize_radius_gateway_url, RadiusOAuth};
pub use shared::{parse_authorization_input, AuthorizationInput};
pub use xai::XaiOAuth;
