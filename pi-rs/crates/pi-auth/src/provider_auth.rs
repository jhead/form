//! The two auth methods a provider can offer, as object-safe traits.
//!
//! Upstream expresses these as structural interfaces with closures
//! (`ApiKeyAuth`, `OAuthAuth`, `ProviderAuth` in `auth/types.ts`). Here they are
//! `Arc<dyn …>` so the set is extensible from Swift without generics leaking
//! into public signatures.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::options::AbortSignal;

use crate::context::AuthContext;
use crate::error::AuthError;
use crate::interaction::LoginContext;
use crate::types::{ApiKeyCredential, AuthCheck, AuthResult, ModelAuth, OAuthCredential};

/// Inputs to [`ApiKeyAuth::resolve`] / [`ApiKeyAuth::check`].
#[derive(Clone)]
pub struct ApiKeyAuthInput {
    pub ctx: Arc<dyn AuthContext>,
    pub credential: Option<ApiKeyCredential>,
    pub signal: AbortSignal,
}

impl ApiKeyAuthInput {
    pub fn new(ctx: Arc<dyn AuthContext>) -> Self {
        Self {
            ctx,
            credential: None,
            signal: AbortSignal::never(),
        }
    }

    pub fn throw_if_aborted(&self) -> Result<(), AuthError> {
        if self.signal.is_aborted() {
            return Err(AuthError::Cancelled);
        }
        Ok(())
    }
}

/// Api-key auth: a stored key/provider env plus ambient sources (env vars, AWS
/// profiles, ADC files). Ambient-only providers leave `login` unimplemented.
#[async_trait]
pub trait ApiKeyAuth: Send + Sync {
    /// Display name, e.g. `"Anthropic API key"`.
    fn name(&self) -> &str;

    /// Whether [`login`](Self::login) can do anything. `false` = ambient-only.
    fn supports_login(&self) -> bool {
        false
    }

    /// Interactive setup (prompt for the key / provider env).
    async fn login(&self, ctx: &LoginContext) -> Result<ApiKeyCredential, AuthError> {
        let _ = ctx;
        Err(AuthError::auth(format!(
            "{} has no interactive api-key login",
            self.name()
        )))
    }

    /// Optional side-effect-free availability check, for when `resolve` may run
    /// commands or otherwise do request-time work. `None` means "ask resolve".
    async fn check(&self, input: ApiKeyAuthInput) -> Result<Option<AuthCheck>, AuthError> {
        let _ = input;
        Ok(None)
    }

    /// Resolve auth from the stored credential and/or ambient sources, merging
    /// per field. `None` = not configured.
    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>, AuthError>;
}

/// OAuth auth. The `refresh` / `to_auth` split is what lets the resolver own
/// the locked single-flight refresh: `refresh` produces a credential,
/// `to_auth` derives request auth from whatever ends up stored.
#[async_trait]
pub trait OAuthFlow: Send + Sync {
    /// Display name, e.g. `"Anthropic (Claude Pro/Max)"`.
    fn name(&self) -> &str;

    /// Whether access through this method is backed by a subscription.
    fn is_subscription(&self) -> bool {
        false
    }

    /// Selector label, e.g. `"Sign in with SuperGrok or X Premium"`.
    fn login_label(&self) -> Option<&str> {
        None
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError>;

    /// Exchange the refresh token. Network call; fails on `invalid_grant` etc.
    /// The resolver runs this under the credential-store lock.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError>;

    /// Side-effect-free derivation of request auth from a valid credential.
    /// Covers per-credential `baseUrl` (GitHub Copilot).
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError>;
}

/// Provider auth. At least one of `api_key` / `oauth` must be present.
#[derive(Clone, Default)]
pub struct ProviderAuth {
    pub api_key: Option<Arc<dyn ApiKeyAuth>>,
    pub oauth: Option<Arc<dyn OAuthFlow>>,
}

impl ProviderAuth {
    pub fn api_key(auth: Arc<dyn ApiKeyAuth>) -> Self {
        Self {
            api_key: Some(auth),
            oauth: None,
        }
    }

    pub fn oauth(flow: Arc<dyn OAuthFlow>) -> Self {
        Self {
            api_key: None,
            oauth: Some(flow),
        }
    }

    pub fn with_oauth(mut self, flow: Arc<dyn OAuthFlow>) -> Self {
        self.oauth = Some(flow);
        self
    }

    pub fn with_api_key(mut self, auth: Arc<dyn ApiKeyAuth>) -> Self {
        self.api_key = Some(auth);
        self
    }
}

impl std::fmt::Debug for ProviderAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAuth")
            .field("api_key", &self.api_key.as_ref().map(|a| a.name()))
            .field("oauth", &self.oauth.as_ref().map(|a| a.name()))
            .finish()
    }
}

/// A provider as far as auth resolution is concerned.
#[derive(Clone, Debug)]
pub struct AuthProvider {
    pub id: String,
    pub auth: ProviderAuth,
}

impl AuthProvider {
    pub fn new(id: impl Into<String>, auth: ProviderAuth) -> Self {
        Self {
            id: id.into(),
            auth,
        }
    }
}
