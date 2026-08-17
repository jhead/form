//! Credential resolution: env vars, the on-disk credential store, and the
//! OAuth/device-code flows for subscription providers.
//!
//! Port of `.upstream/packages/ai/src/auth/` and `env-api-keys.ts`, plus the
//! `auth.json` store from `packages/coding-agent/src/core/auth-storage.ts`.
//!
//! # Entry points
//!
//! - [`resolve_provider_auth`] — the one call a provider adapter makes. Applies
//!   the upstream resolution order and refreshes an expiring OAuth token
//!   transparently, once, however many callers race.
//! - [`FileCredentialStore`] / [`InMemoryCredentialStore`] — the
//!   [`CredentialStore`] implementations. The file store reads and writes the
//!   same `~/.pi/agent/auth.json` the TypeScript CLI uses.
//! - [`OAuthFlows`] / [`load_oauth_flow`] — the built-in provider flows.
//! - [`find_env_keys`] / [`get_env_api_key`] — environment discovery.
//!
//! # Interaction is the host's job
//!
//! This crate never touches stdin or stdout and never opens a browser. Login
//! flows talk to the user only through `Arc<dyn `[`AuthInteraction`]`>`, which
//! the Swift host implements; [`HeadlessAuthInteraction`] is the default and
//! fails prompts rather than blocking. A flow binds a local OAuth callback
//! socket only when the caller opts in with
//! [`LoginContext::with_local_callback_server`].
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use pi_auth::*;
//! # async fn example() -> Result<(), AuthError> {
//! let credentials: Arc<dyn CredentialStore> = Arc::new(FileCredentialStore::open_default());
//! let provider = AuthProvider::new(
//!     "anthropic",
//!     ProviderAuth::api_key(EnvApiKeyAuth::shared("Anthropic API key", ["ANTHROPIC_API_KEY"]))
//!         .with_oauth(OAuthFlows::default().anthropic()),
//! );
//!
//! let resolved = resolve_provider_auth(
//!     &provider,
//!     credentials,
//!     DefaultAuthContext::shared(),
//!     &AuthResolutionOverrides::default(),
//! )
//! .await?;
//!
//! if let Some(result) = resolved {
//!     println!("authenticated via {}", result.source.unwrap_or_default());
//! }
//! # Ok(())
//! # }
//! ```

pub mod config_value;
pub mod context;
pub mod credential_store;
pub mod env_api_keys;
pub mod error;
pub mod helpers;
pub mod http;
pub mod interaction;
pub mod oauth;
pub mod provider_auth;
pub mod resolve;
pub mod types;

pub use config_value::{
    clear_config_value_cache, config_value_env_var_names, is_command_config_value,
    resolve_config_value, resolve_config_value_uncached,
};
pub use context::{overlay_env, AuthContext, DefaultAuthContext, MapAuthContext};
pub use credential_store::{
    default_agent_dir, default_auth_path, mutation_fn, read_stored_credential, set_credential,
    CredentialMutation, CredentialStore, FileCredentialStore, InMemoryCredentialStore,
    ENV_AGENT_DIR,
};
pub use env_api_keys::{
    api_key_env_vars, find_env_keys, get_env_api_key, provider_env_value, ANTHROPIC_API_KEY_ENV,
    ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_OAUTH_TOKEN_ENV, AUTHENTICATED_SENTINEL,
};
pub use error::AuthError;
pub use helpers::EnvApiKeyAuth;
pub use http::OAuthHttp;
pub use interaction::{AuthInteraction, HeadlessAuthInteraction, LoginContext};
pub use oauth::{
    load_oauth_flow, register_oauth_flow, unregister_oauth_flow, AnthropicOAuth,
    GitHubCopilotOAuth, KimiCodingOAuth, OAuthFlows, OpenAICodexOAuth, OpenRouterOAuth,
    RadiusOAuth, XaiOAuth, BUILT_IN_OAUTH_PROVIDERS,
};
pub use provider_auth::{ApiKeyAuth, ApiKeyAuthInput, AuthProvider, OAuthFlow, ProviderAuth};
pub use resolve::{
    resolve_provider_auth, AuthResolutionOverrides, DEFAULT_OAUTH_MINIMUM_VALIDITY_MS,
};
pub use types::{
    ApiKeyCredential, AuthCheck, AuthEvent, AuthInfoLink, AuthResult, AuthType, Credential,
    CredentialInfo, DeviceCode, ModelAuth, OAuthCredential, SelectOption, SelectPrompt, TextPrompt,
};
