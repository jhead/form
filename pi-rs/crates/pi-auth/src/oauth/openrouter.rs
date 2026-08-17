//! Port of `packages/ai/src/auth/oauth/openrouter.ts`.
//!
//! OpenRouter exchanges the authorization code for a *permanent*,
//! user-controlled API key rather than an expiring access/refresh pair — hence
//! the sentinel expiry and the no-op refresh.

use std::time::Duration;

use async_trait::async_trait;
use pi_core::options::AbortSignal;
use serde_json::{json, Value};

use crate::error::AuthError;
use crate::http::OAuthHttp;
use crate::interaction::LoginContext;
use crate::oauth::callback_server::{CallbackPage, LoopbackCallback};
use crate::oauth::pkce::generate_pkce;
use crate::oauth::shared::{await_redirect, RedirectOutcome};
use crate::provider_auth::OAuthFlow;
use crate::types::{AuthEvent, ModelAuth, OAuthCredential, TextPrompt};

const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
/// `Number.MAX_SAFE_INTEGER`, so the key never looks expired to the resolver.
const NEVER_EXPIRES: i64 = 9_007_199_254_740_991;

pub struct OpenRouterOAuth {
    http: OAuthHttp,
    authorize_url: String,
    token_url: String,
}

impl Default for OpenRouterOAuth {
    fn default() -> Self {
        Self::new(OAuthHttp::default())
    }
}

impl OpenRouterOAuth {
    pub fn new(http: OAuthHttp) -> Self {
        Self {
            http,
            authorize_url: AUTHORIZE_URL.to_string(),
            token_url: TOKEN_URL.to_string(),
        }
    }

    pub fn with_urls(
        mut self,
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        self.authorize_url = authorize_url.into();
        self.token_url = token_url.into();
        self
    }

    async fn exchange_authorization_code(
        &self,
        code: &str,
        verifier: &str,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        if signal.is_aborted() {
            return Err(AuthError::Cancelled);
        }

        let response = self
            .http
            .post_json(
                &self.token_url,
                &json!({
                    "code": code,
                    "code_verifier": verifier,
                    "code_challenge_method": "S256",
                }),
                &[],
                signal,
                Some(TOKEN_EXCHANGE_TIMEOUT),
            )
            .await
            .map_err(|error| match error {
                AuthError::TimedOut { .. } => {
                    AuthError::oauth("OpenRouter OAuth token exchange timed out")
                }
                other => other,
            })?;

        let body = response.json_object();
        if !response.ok() {
            let detail = body
                .as_ref()
                .and_then(error_detail)
                .map(|d| format!(": {d}"))
                .unwrap_or_default();
            return Err(AuthError::oauth(format!(
                "OpenRouter OAuth key exchange failed (HTTP {}){detail}",
                response.status
            )));
        }

        let Some(body) = body else {
            return Err(AuthError::oauth("OpenRouter OAuth returned invalid JSON"));
        };
        let Some(key) = body
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
        else {
            return Err(AuthError::oauth(
                "OpenRouter OAuth response carries no \"key\"",
            ));
        };

        Ok(OAuthCredential::new(key, "", NEVER_EXPIRES))
    }
}

fn error_detail(body: &serde_json::Map<String, Value>) -> Option<String> {
    for field in ["error_description", "message"] {
        if let Some(value) = body.get(field).and_then(Value::as_str) {
            return Some(value.to_string());
        }
    }
    match body.get("error") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Object(error)) => error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

#[async_trait]
impl OAuthFlow for OpenRouterOAuth {
    fn name(&self) -> &str {
        "OpenRouter OAuth"
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with OpenRouter")
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let pkce = generate_pkce();
        // An unguessable one-shot path stands in for the `state` parameter,
        // which OpenRouter's callback contract does not carry.
        let callback_path = format!("/oauth/callback/{}", uuid::Uuid::new_v4());

        let mut callback = if ctx.local_callback_server {
            Some(
                LoopbackCallback::bind_default(
                    0,
                    &callback_path,
                    CallbackPage {
                        success_message: "Signed in to OpenRouter. You may now close this page."
                            .into(),
                        expected_state: None,
                    },
                )
                .await?,
            )
        } else {
            None
        };

        let callback_url = callback
            .as_ref()
            .map(|c| c.redirect_uri().to_string())
            .unwrap_or_default();

        let mut authorize_url = url::Url::parse(&self.authorize_url)
            .map_err(|e| AuthError::oauth(format!("invalid authorize url: {e}")))?;
        {
            let mut pairs = authorize_url.query_pairs_mut();
            if !callback_url.is_empty() {
                pairs.append_pair("callback_url", &callback_url);
            }
            pairs
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256");
        }

        if !callback_url.is_empty() {
            ctx.progress(format!(
                "Listening for OpenRouter OAuth callback on {callback_url}"
            ));
        }
        ctx.emit(AuthEvent::AuthUrl {
            url: authorize_url.to_string(),
            instructions: Some(
                "Complete sign-in in your browser. If the browser is on another machine, paste the final redirect URL here."
                    .into(),
            ),
        });

        let prompt = TextPrompt::new(
            "Complete sign-in in your browser, or paste the authorization code / redirect URL here:",
        );
        let prompt = if callback_url.is_empty() {
            prompt
        } else {
            prompt.placeholder(callback_url)
        };

        let code = match await_redirect(ctx, callback.as_mut(), prompt).await? {
            RedirectOutcome::Callback(params) => params.get("code").cloned(),
            RedirectOutcome::Manual(input) => input.code,
        };
        let code = code.ok_or_else(|| AuthError::oauth("Missing authorization code"))?;

        ctx.progress("Exchanging authorization code for an API key...");
        self.exchange_authorization_code(&code, &pkce.verifier, &ctx.signal)
            .await
    }

    /// The minted key is permanent; there is nothing to exchange.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        Ok(credential.clone())
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::from_api_key(credential.access.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn derives_the_api_key_and_keeps_the_permanent_credential_on_refresh() {
        let flow = OpenRouterOAuth::default();
        let credential = OAuthCredential::new("token", "", NEVER_EXPIRES);

        assert_eq!(
            flow.to_auth(&credential).await.unwrap(),
            ModelAuth::from_api_key("token")
        );
        assert_eq!(
            flow.refresh(&credential, AbortSignal::never())
                .await
                .unwrap(),
            credential
        );
    }

    #[test]
    fn openrouter_is_not_a_subscription_flow() {
        assert!(!OpenRouterOAuth::default().is_subscription());
    }

    #[test]
    fn error_details_come_from_whichever_field_the_server_used() {
        let from_description = json!({ "error_description": "bad code" });
        let from_message = json!({ "message": "nope" });
        let from_nested = json!({ "error": { "message": "deep" } });
        let from_string = json!({ "error": "flat" });

        for (body, expected) in [
            (from_description, "bad code"),
            (from_message, "nope"),
            (from_nested, "deep"),
            (from_string, "flat"),
        ] {
            let map = body.as_object().unwrap().clone();
            assert_eq!(error_detail(&map).as_deref(), Some(expected));
        }
    }
}
