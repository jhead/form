//! Port of `packages/ai/src/auth/resolve.ts`.
//!
//! Resolution order, verbatim from upstream: an explicit `api_key` override,
//! then the stored credential (OAuth or api-key), and only if *nothing* is
//! stored does ambient resolution run — which is where per-provider environment
//! variables live, via [`EnvApiKeyAuth`](crate::EnvApiKeyAuth). Note the
//! consequence: a stored credential owns the provider. There is no silent env
//! fallback after a failed refresh, or for a credential type the provider has
//! no handler for.

use std::sync::Arc;
use std::time::Duration;

use pi_core::message::now_ms;
use pi_core::options::{AbortSignal, ProviderEnv};

use crate::context::{overlay_env, AuthContext};
use crate::credential_store::{mutation_fn, CredentialStore};
use crate::error::AuthError;
use crate::provider_auth::{ApiKeyAuth, ApiKeyAuthInput, AuthProvider, OAuthFlow};
use crate::types::{ApiKeyCredential, AuthResult, Credential, OAuthCredential};

/// Refresh a token that has less than this long to live.
pub const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: i64 = 5 * 60 * 1000;
const DEFAULT_OAUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
pub struct AuthResolutionOverrides {
    /// Caller-supplied key; bypasses the store entirely.
    pub api_key: Option<String>,
    /// Provider-scoped env overlaid on the ambient context.
    pub env: ProviderEnv,
    /// Require this much remaining OAuth validity. Defaults to five minutes;
    /// setting it explicitly also makes a too-short refreshed token an error.
    pub min_oauth_validity_ms: Option<i64>,
    pub signal: Option<AbortSignal>,
}

impl AuthResolutionOverrides {
    fn signal(&self) -> AbortSignal {
        self.signal.clone().unwrap_or_default()
    }
}

impl std::fmt::Debug for AuthResolutionOverrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthResolutionOverrides")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("min_oauth_validity_ms", &self.min_oauth_validity_ms)
            .finish_non_exhaustive()
    }
}

/// Resolve request auth for one provider, refreshing an expiring OAuth token
/// transparently and exactly once across concurrent callers.
pub async fn resolve_provider_auth(
    provider: &AuthProvider,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
    overrides: &AuthResolutionOverrides,
) -> Result<Option<AuthResult>, AuthError> {
    let signal = overrides.signal();
    if signal.is_aborted() {
        return Err(AuthError::Cancelled);
    }

    let request_context = if overrides.env.is_empty() {
        auth_context
    } else {
        overlay_env(auth_context, overrides.env.clone())
    };

    if let (Some(api_key), Some(auth)) =
        (overrides.api_key.as_ref(), provider.auth.api_key.as_ref())
    {
        let credential = ApiKeyCredential {
            key: Some(api_key.clone()),
            env: overrides.env.clone(),
        };
        return resolve_api_key(
            request_context,
            auth.as_ref(),
            &provider.id,
            Some(credential),
            signal,
        )
        .await;
    }

    let stored = credentials
        .read(&provider.id, Some(signal.clone()))
        .await
        .map_err(|error| match error {
            AuthError::Cancelled => error,
            other => AuthError::auth(format!(
                "Credential store read failed for {}: {other}",
                provider.id
            )),
        })?;

    if let Some(stored) = stored {
        return match stored {
            Credential::OAuth(stored) => match provider.auth.oauth.as_ref() {
                Some(oauth) => {
                    resolve_stored_oauth(
                        credentials,
                        &provider.id,
                        oauth.clone(),
                        stored,
                        signal,
                        overrides.min_oauth_validity_ms,
                    )
                    .await
                }
                // A credential type without a matching handler resolves to
                // nothing rather than silently falling back to the env.
                None => Ok(None),
            },
            Credential::ApiKey(stored) => match provider.auth.api_key.as_ref() {
                Some(auth) => {
                    let credential = if overrides.env.is_empty() {
                        stored
                    } else {
                        let mut merged = stored;
                        merged.env.extend(overrides.env.clone());
                        merged
                    };
                    resolve_api_key(
                        request_context,
                        auth.as_ref(),
                        &provider.id,
                        Some(credential),
                        signal,
                    )
                    .await
                }
                None => Ok(None),
            },
        };
    }

    // Ambient: env vars, AWS profiles, ADC files.
    match provider.auth.api_key.as_ref() {
        Some(auth) => {
            resolve_api_key(request_context, auth.as_ref(), &provider.id, None, signal).await
        }
        None => Ok(None),
    }
}

/// Double-checked locking: a token inside the validity window takes the store
/// lock, re-reads under it, refreshes once globally, and persists the rotated
/// credential before release. Concurrent callers queue on the same lock and
/// find the fresh token already committed, so only one refresh goes out.
async fn resolve_stored_oauth(
    credentials: Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: Arc<dyn OAuthFlow>,
    stored: OAuthCredential,
    signal: AbortSignal,
    min_oauth_validity_ms: Option<i64>,
) -> Result<Option<AuthResult>, AuthError> {
    let minimum_validity_ms =
        DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_oauth_validity_ms.unwrap_or(0));
    let expires_soon = move |credential: &OAuthCredential| -> bool {
        now_ms() + minimum_validity_ms >= credential.expires
    };

    let mut credential = stored;

    if expires_soon(&credential) {
        let refresh_flow = oauth.clone();
        let refresh_signal = signal.clone();
        let owner = provider_id.to_string();

        let mutation = mutation_fn(move |current| {
            let flow = refresh_flow.clone();
            let signal = refresh_signal.clone();
            let owner = owner.clone();
            async move {
                // Logged out, or replaced by an api-key credential, meanwhile.
                let Some(Credential::OAuth(current)) = current else {
                    return Ok(None);
                };
                // Another process or request already refreshed it.
                if !expires_soon(&current) {
                    return Ok(None);
                }
                let refreshed = tokio::time::timeout(
                    DEFAULT_OAUTH_REFRESH_TIMEOUT,
                    flow.refresh(&current, signal),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(AuthError::timed_out(
                        "OAuth refresh timed out after 15000ms",
                    ))
                })
                .map_err(|error| match error {
                    AuthError::Cancelled => error,
                    other => AuthError::oauth(format!("OAuth refresh failed for {owner}: {other}")),
                })?;
                Ok(Some(Credential::OAuth(refreshed)))
            }
        });

        let post = credentials
            .modify(provider_id, mutation, Some(signal.clone()))
            .await
            .map_err(|error| match error {
                AuthError::Cancelled | AuthError::OAuth { .. } => error,
                other => AuthError::auth(format!(
                    "Credential store modify failed for {provider_id}: {other}"
                )),
            })?;

        let Some(Credential::OAuth(post)) = post else {
            // Logged out meanwhile.
            return Ok(None);
        };
        credential = post;

        // The default five-minute window triggers a refresh but does not impose
        // a provider contract. Explicit callers (bearer-token export) do require
        // the requested minimum after the refresh.
        if min_oauth_validity_ms.is_some() && expires_soon(&credential) {
            return Err(AuthError::oauth(format!(
                "OAuth refresh returned a token that expires too soon for {provider_id}"
            )));
        }
    }

    let auth = oauth
        .to_auth(&credential)
        .await
        .map_err(|error| match error {
            AuthError::Cancelled => error,
            other => AuthError::oauth(format!(
                "OAuth auth derivation failed for {provider_id}: {other}"
            )),
        })?;

    Ok(Some(AuthResult {
        auth,
        env: Default::default(),
        source: Some("OAuth".to_string()),
    }))
}

async fn resolve_api_key(
    ctx: Arc<dyn AuthContext>,
    api_key: &dyn ApiKeyAuth,
    provider_id: &str,
    credential: Option<ApiKeyCredential>,
    signal: AbortSignal,
) -> Result<Option<AuthResult>, AuthError> {
    api_key
        .resolve(ApiKeyAuthInput {
            ctx,
            credential,
            signal,
        })
        .await
        .map_err(|error| match error {
            AuthError::Cancelled => error,
            other => AuthError::auth(format!(
                "API key auth failed for provider {provider_id}: {other}"
            )),
        })
}
