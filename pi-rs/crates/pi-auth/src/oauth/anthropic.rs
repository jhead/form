//! Port of `packages/ai/src/auth/oauth/anthropic.ts` — Claude Pro/Max.

use async_trait::async_trait;
use pi_core::options::AbortSignal;
use serde_json::json;

use crate::error::AuthError;
use crate::http::{num_field, str_field, OAuthHttp};
use crate::interaction::LoginContext;
use crate::oauth::callback_server::{CallbackPage, LoopbackCallback};
use crate::oauth::pkce::generate_pkce;
use crate::oauth::shared::{await_redirect, expires_at, RedirectOutcome};
use crate::provider_auth::OAuthFlow;
use crate::types::{AuthEvent, ModelAuth, OAuthCredential, TextPrompt};

/// Base64 of the client id, exactly as upstream stores it.
const CLIENT_ID_B64: &str = "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CALLBACK_PORT: u16 = 53692;
const CALLBACK_PATH: &str = "/callback";
/// Registered with the provider verbatim; the bind address may differ but this
/// string is what goes on the wire.
const REDIRECT_URI: &str = "http://localhost:53692/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Refresh a little before the reported expiry.
const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;

fn client_id() -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD
        .decode(CLIENT_ID_B64)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .expect("client id decodes")
}

pub struct AnthropicOAuth {
    http: OAuthHttp,
    authorize_url: String,
    token_url: String,
}

impl Default for AnthropicOAuth {
    fn default() -> Self {
        Self::new(OAuthHttp::default())
    }
}

impl AnthropicOAuth {
    pub fn new(http: OAuthHttp) -> Self {
        Self {
            http,
            authorize_url: AUTHORIZE_URL.to_string(),
            token_url: TOKEN_URL.to_string(),
        }
    }

    /// Point the flow at different endpoints (tests, staging).
    pub fn with_urls(
        mut self,
        authorize_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        self.authorize_url = authorize_url.into();
        self.token_url = token_url.into();
        self
    }

    async fn post_token(
        &self,
        body: serde_json::Value,
        signal: &AbortSignal,
        what: &str,
    ) -> Result<OAuthCredential, AuthError> {
        let response = self
            .http
            .post_json(&self.token_url, &body, &[], signal, None)
            .await
            .map_err(|error| match error {
                AuthError::Cancelled => error,
                other => AuthError::oauth(format!(
                    "Anthropic {what} request failed. url={}; details={other}",
                    self.token_url
                )),
            })?;

        if !response.ok() {
            return Err(AuthError::oauth(format!(
                "HTTP request failed. status={}; url={}; body={}",
                response.status, self.token_url, response.body
            )));
        }

        let Some(json) = response.json_object() else {
            return Err(AuthError::oauth(format!(
                "Anthropic {what} returned invalid JSON. url={}; body={}",
                self.token_url, response.body
            )));
        };

        let (Some(access), Some(refresh), Some(expires_in)) = (
            str_field(&json, "access_token"),
            str_field(&json, "refresh_token"),
            num_field(&json, "expires_in"),
        ) else {
            return Err(AuthError::oauth(format!(
                "Anthropic {what} response missing fields: {}",
                response.body
            )));
        };

        Ok(OAuthCredential::new(
            access,
            refresh,
            expires_at(expires_in, EXPIRY_SKEW_MS),
        ))
    }
}

#[async_trait]
impl OAuthFlow for AnthropicOAuth {
    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let pkce = generate_pkce();
        // Upstream reuses the verifier as the state parameter.
        let state = pkce.verifier.clone();

        let mut callback = if ctx.local_callback_server {
            Some(
                LoopbackCallback::bind_default(
                    CALLBACK_PORT,
                    CALLBACK_PATH,
                    CallbackPage {
                        success_message:
                            "Anthropic authentication completed. You can close this window.".into(),
                        expected_state: Some(state.clone()),
                    },
                )
                .await?,
            )
        } else {
            None
        };

        let auth_url = {
            let mut url = url::Url::parse(&self.authorize_url)
                .map_err(|e| AuthError::oauth(format!("invalid authorize url: {e}")))?;
            url.query_pairs_mut()
                .append_pair("code", "true")
                .append_pair("client_id", &client_id())
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", REDIRECT_URI)
                .append_pair("scope", SCOPES)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state);
            url.to_string()
        };

        ctx.emit(AuthEvent::AuthUrl {
            url: auth_url,
            instructions: Some(
                "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here."
                    .into(),
            ),
        });

        let outcome = await_redirect(
            ctx,
            callback.as_mut(),
            TextPrompt::new(
                "Complete login in your browser, or paste the authorization code / redirect URL here:",
            )
            .placeholder(REDIRECT_URI),
        )
        .await?;

        let (code, returned_state) = match outcome {
            RedirectOutcome::Callback(params) => (
                params.get("code").cloned(),
                params.get("state").cloned().or_else(|| Some(state.clone())),
            ),
            RedirectOutcome::Manual(input) => {
                if let Some(returned) = input.state.as_ref() {
                    if returned != &state {
                        return Err(AuthError::oauth("OAuth state mismatch"));
                    }
                }
                let returned_state = input.state.clone().or_else(|| Some(state.clone()));
                (input.code, returned_state)
            }
        };

        let code = code.ok_or_else(|| AuthError::oauth("Missing authorization code"))?;
        let returned_state =
            returned_state.ok_or_else(|| AuthError::oauth("Missing OAuth state"))?;

        ctx.progress("Exchanging authorization code for tokens...");
        self.post_token(
            json!({
                "grant_type": "authorization_code",
                "client_id": client_id(),
                "code": code,
                "state": returned_state,
                "redirect_uri": REDIRECT_URI,
                "code_verifier": pkce.verifier,
            }),
            &ctx.signal,
            "token exchange",
        )
        .await
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        // No `scope` on refresh: the provider rejects it.
        self.post_token(
            json!({
                "grant_type": "refresh_token",
                "client_id": client_id(),
                "refresh_token": credential.refresh,
            }),
            &signal,
            "token refresh",
        )
        .await
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::from_api_key(credential.access.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_id_decodes_to_the_registered_uuid() {
        assert_eq!(client_id(), "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    #[tokio::test]
    async fn to_auth_derives_the_api_key_from_the_access_token() {
        let flow = AnthropicOAuth::default();
        let auth = flow
            .to_auth(&OAuthCredential::new("token", "r", 0))
            .await
            .unwrap();
        assert_eq!(auth, ModelAuth::from_api_key("token"));
    }
}
