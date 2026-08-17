//! Port of `packages/ai/src/auth/oauth/openai-codex.ts` — ChatGPT Plus/Pro.

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine;
use pi_core::message::now_ms;
use pi_core::options::AbortSignal;
use rand::RngCore;
use serde_json::{json, Value};

use crate::error::AuthError;
use crate::http::{num_field, str_field, OAuthHttp};
use crate::interaction::LoginContext;
use crate::oauth::callback_server::{CallbackPage, LoopbackCallback};
use crate::oauth::device_code::{poll_device_code_flow, DeviceCodePollOptions, PollResult};
use crate::oauth::pkce::generate_pkce;
use crate::oauth::shared::{await_redirect, RedirectOutcome};
use crate::provider_auth::OAuthFlow;
use crate::types::{
    AuthEvent, DeviceCode, ModelAuth, OAuthCredential, SelectOption, SelectPrompt, TextPrompt,
};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL: &str = "https://auth.openai.com";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
const BROWSER_LOGIN_METHOD: &str = "browser";
const DEVICE_CODE_LOGIN_METHOD: &str = "device_code";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

pub struct OpenAICodexOAuth {
    http: OAuthHttp,
    auth_base_url: String,
}

impl Default for OpenAICodexOAuth {
    fn default() -> Self {
        Self::new(OAuthHttp::default())
    }
}

struct DeviceAuthInfo {
    device_auth_id: String,
    user_code: String,
    interval_seconds: f64,
}

struct Token {
    access: String,
    refresh: String,
    expires: i64,
}

impl OpenAICodexOAuth {
    pub fn new(http: OAuthHttp) -> Self {
        Self {
            http,
            auth_base_url: AUTH_BASE_URL.to_string(),
        }
    }

    /// Point the flow at a different `auth.openai.com` (tests, staging).
    pub fn with_auth_base_url(mut self, base: impl Into<String>) -> Self {
        self.auth_base_url = base.into().trim_end_matches('/').to_string();
        self
    }

    fn authorize_url(&self) -> String {
        format!("{}/oauth/authorize", self.auth_base_url)
    }

    fn token_url(&self) -> String {
        format!("{}/oauth/token", self.auth_base_url)
    }

    fn device_user_code_url(&self) -> String {
        format!("{}/api/accounts/deviceauth/usercode", self.auth_base_url)
    }

    fn device_token_url(&self) -> String {
        format!("{}/api/accounts/deviceauth/token", self.auth_base_url)
    }

    fn device_verification_uri(&self) -> String {
        format!("{}/codex/device", self.auth_base_url)
    }

    fn device_redirect_uri(&self) -> String {
        format!("{}/deviceauth/callback", self.auth_base_url)
    }

    async fn read_token_response(
        &self,
        body: &[(&str, &str)],
        signal: &AbortSignal,
        operation: &str,
    ) -> Result<Token, AuthError> {
        let response = self
            .http
            .post_form(&self.token_url(), body, &[], signal, None)
            .await
            .map_err(|error| match error {
                AuthError::Cancelled => error,
                other if operation == "refresh" => {
                    AuthError::oauth(format!("OpenAI Codex token refresh error: {other}"))
                }
                other => other,
            })?;

        if !response.ok() {
            let detail = if response.body.is_empty() {
                response.status.to_string()
            } else {
                response.body.clone()
            };
            return Err(AuthError::oauth(format!(
                "OpenAI Codex token {operation} failed ({}): {detail}",
                response.status
            )));
        }

        let json = response.json_object().unwrap_or_default();
        let (Some(access), Some(refresh), Some(expires_in)) = (
            str_field(&json, "access_token"),
            str_field(&json, "refresh_token"),
            num_field(&json, "expires_in"),
        ) else {
            return Err(AuthError::oauth(format!(
                "OpenAI Codex token {operation} response missing fields: {}",
                response.body
            )));
        };

        Ok(Token {
            access,
            refresh,
            expires: now_ms() + (expires_in * 1000.0) as i64,
        })
    }

    async fn exchange_authorization_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let token = self
            .read_token_response(
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", CLIENT_ID),
                    ("code", code),
                    ("code_verifier", verifier),
                    ("redirect_uri", redirect_uri),
                ],
                signal,
                "exchange",
            )
            .await?;
        credentials_from_token(token)
    }

    async fn start_device_auth(&self, signal: &AbortSignal) -> Result<DeviceAuthInfo, AuthError> {
        let response = self
            .http
            .post_json(
                &self.device_user_code_url(),
                &json!({ "client_id": CLIENT_ID }),
                &[],
                signal,
                None,
            )
            .await?;

        if !response.ok() {
            if response.status == 404 {
                return Err(AuthError::oauth(
                    "OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL.",
                ));
            }
            let suffix = if response.body.is_empty() {
                String::new()
            } else {
                format!(": {}", response.body)
            };
            return Err(AuthError::oauth(format!(
                "OpenAI Codex device code request failed with status {}{suffix}",
                response.status
            )));
        }

        let json = response.json_object().unwrap_or_default();
        let interval_seconds = num_field(&json, "interval");
        let (Some(device_auth_id), Some(user_code), Some(interval_seconds)) = (
            str_field(&json, "device_auth_id"),
            str_field(&json, "user_code"),
            interval_seconds.filter(|i| i.is_finite() && *i >= 0.0),
        ) else {
            return Err(AuthError::oauth(format!(
                "Invalid OpenAI Codex device code response: {}",
                response.body
            )));
        };

        Ok(DeviceAuthInfo {
            device_auth_id,
            user_code,
            interval_seconds,
        })
    }

    async fn poll_device_auth(
        &self,
        device: &DeviceAuthInfo,
        signal: &AbortSignal,
    ) -> Result<(String, String), AuthError> {
        let url = self.device_token_url();
        poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(device.interval_seconds),
                expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
                wait_before_first_poll: false,
                signal: signal.clone(),
            },
            || async {
                let response = self
                    .http
                    .post_json(
                        &url,
                        &json!({
                            "device_auth_id": device.device_auth_id,
                            "user_code": device.user_code,
                        }),
                        &[],
                        signal,
                        None,
                    )
                    .await?;

                if response.ok() {
                    let json = response.json_object().unwrap_or_default();
                    let (Some(code), Some(verifier)) = (
                        str_field(&json, "authorization_code"),
                        str_field(&json, "code_verifier"),
                    ) else {
                        return Ok(PollResult::Failed {
                            message: format!(
                                "Invalid OpenAI Codex device auth token response: {}",
                                response.body
                            ),
                        });
                    };
                    return Ok(PollResult::Complete((code, verifier)));
                }

                // The endpoint answers 403/404 while the user has not finished.
                if response.status == 403 || response.status == 404 {
                    return Ok(PollResult::Pending);
                }

                let error_code = response
                    .json_object()
                    .and_then(|json| match json.get("error") {
                        Some(Value::String(code)) => Some(code.clone()),
                        Some(Value::Object(error)) => error
                            .get("code")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        _ => None,
                    });

                Ok(match error_code.as_deref() {
                    Some("deviceauth_authorization_pending") => PollResult::Pending,
                    Some("slow_down") => PollResult::SlowDown {
                        interval_seconds: None,
                    },
                    _ => {
                        let suffix = if response.body.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", response.body)
                        };
                        PollResult::Failed {
                            message: format!(
                                "OpenAI Codex device auth failed with status {}{suffix}",
                                response.status
                            ),
                        }
                    }
                })
            },
        )
        .await
    }

    async fn login_device_code(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        let device = self.start_device_auth(&ctx.signal).await?;
        ctx.emit(AuthEvent::DeviceCode(DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: self.device_verification_uri(),
            interval_seconds: Some(device.interval_seconds as u64),
            expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS as u64),
        }));

        let (code, verifier) = self.poll_device_auth(&device, &ctx.signal).await?;
        self.exchange_authorization_code(&code, &verifier, &self.device_redirect_uri(), &ctx.signal)
            .await
    }

    async fn login_browser(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        let pkce = generate_pkce();
        let state = random_state();

        let mut callback = if ctx.local_callback_server {
            Some(
                LoopbackCallback::bind_default(
                    CALLBACK_PORT,
                    CALLBACK_PATH,
                    CallbackPage {
                        success_message:
                            "OpenAI authentication completed. You can close this window.".into(),
                        expected_state: Some(state.clone()),
                    },
                )
                .await?,
            )
        } else {
            None
        };

        let url = {
            let mut url = url::Url::parse(&self.authorize_url())
                .map_err(|e| AuthError::oauth(format!("invalid authorize url: {e}")))?;
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", CLIENT_ID)
                .append_pair("redirect_uri", REDIRECT_URI)
                .append_pair("scope", SCOPE)
                .append_pair("code_challenge", &pkce.challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state)
                .append_pair("id_token_add_organizations", "true")
                .append_pair("codex_cli_simplified_flow", "true")
                .append_pair("originator", "pi");
            url.to_string()
        };

        ctx.emit(AuthEvent::AuthUrl {
            url,
            instructions: Some("A browser window should open. Complete login to finish.".into()),
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

        let code = match outcome {
            RedirectOutcome::Callback(params) => params.get("code").cloned(),
            RedirectOutcome::Manual(input) => {
                if let Some(returned) = input.state.as_ref() {
                    if returned != &state {
                        return Err(AuthError::oauth("State mismatch"));
                    }
                }
                input.code
            }
        };
        let code = code.ok_or_else(|| AuthError::oauth("Missing authorization code"))?;

        self.exchange_authorization_code(&code, &pkce.verifier, REDIRECT_URI, &ctx.signal)
            .await
    }
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read `chatgpt_account_id` out of the access token. The Codex API keys
/// requests by account, so a token without one is unusable.
fn account_id(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    if access_token.split('.').count() != 3 {
        return None;
    }
    // JWT payloads are base64url, but upstream decodes with `atob`, which also
    // accepts standard base64; accept both rather than reject a live token.
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| STANDARD_NO_PAD.decode(payload))
        .or_else(|_| STANDARD.decode(payload))
        .ok()?;
    let json: Value = serde_json::from_slice(&decoded).ok()?;
    json.get(JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn credentials_from_token(token: Token) -> Result<OAuthCredential, AuthError> {
    let account_id = account_id(&token.access)
        .ok_or_else(|| AuthError::oauth("Failed to extract accountId from token"))?;
    Ok(
        OAuthCredential::new(token.access, token.refresh, token.expires)
            .with_extra("accountId", Value::String(account_id)),
    )
}

#[async_trait]
impl OAuthFlow for OpenAICodexOAuth {
    fn name(&self) -> &str {
        "OpenAI (ChatGPT Plus/Pro)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let method = ctx
            .interaction
            .prompt_select(SelectPrompt {
                message: "Select OpenAI Codex login method:".into(),
                options: vec![
                    SelectOption::new(BROWSER_LOGIN_METHOD, "Browser login (default)"),
                    SelectOption::new(DEVICE_CODE_LOGIN_METHOD, "Device code login (headless)"),
                ],
            })
            .await?;

        match method.as_str() {
            DEVICE_CODE_LOGIN_METHOD => self.login_device_code(ctx).await,
            BROWSER_LOGIN_METHOD => self.login_browser(ctx).await,
            other => Err(AuthError::oauth(format!(
                "Unknown OpenAI Codex login method: {other}"
            ))),
        }
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let token = self
            .read_token_response(
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &credential.refresh),
                    ("client_id", CLIENT_ID),
                ],
                &signal,
                "refresh",
            )
            .await?;
        credentials_from_token(token)
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::from_api_key(credential.access.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn access_token_for(account_id: &str) -> String {
        let header = STANDARD.encode(json!({ "alg": "none" }).to_string());
        let payload = STANDARD
            .encode(json!({ JWT_CLAIM_PATH: { "chatgpt_account_id": account_id } }).to_string());
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn account_id_is_read_from_the_jwt_payload() {
        assert_eq!(
            account_id(&access_token_for("account-123")).as_deref(),
            Some("account-123")
        );
        assert_eq!(account_id("not-a-jwt"), None);
        assert_eq!(account_id("a.b.c"), None);
    }

    #[test]
    fn a_token_without_an_account_id_is_rejected() {
        let token = Token {
            access: "opaque".into(),
            refresh: "r".into(),
            expires: 0,
        };
        let error = credentials_from_token(token).unwrap_err();
        assert!(error.message().contains("accountId"));
    }

    #[tokio::test]
    async fn to_auth_derives_the_api_key_from_the_access_token() {
        let flow = OpenAICodexOAuth::default();
        let auth = flow
            .to_auth(&OAuthCredential::new("token", "r", 0))
            .await
            .unwrap();
        assert_eq!(auth, ModelAuth::from_api_key("token"));
    }

    #[test]
    fn state_values_are_random_hex() {
        let state = random_state();
        assert_eq!(state.len(), 32);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(state, random_state());
    }
}
