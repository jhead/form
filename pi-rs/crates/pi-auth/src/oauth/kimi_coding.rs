//! Port of `packages/ai/src/auth/oauth/kimi-coding.ts`.
//!
//! RFC 8628 device authorization against `https://auth.kimi.com` with JSON
//! responses. The access token authenticates requests to
//! `https://api.kimi.com/coding` as `Authorization: Bearer`, which is why
//! `to_auth` returns a header rather than an api key.

use std::time::Duration;

use async_trait::async_trait;
use pi_core::message::now_ms;
use pi_core::options::AbortSignal;
use serde_json::{Map, Value};

use crate::error::AuthError;
use crate::http::{num_field, str_field, OAuthHttp};
use crate::interaction::LoginContext;
use crate::oauth::device_code::{
    abortable_sleep, poll_device_code_flow, DeviceCodePollOptions, PollResult,
};
use crate::oauth::shared::trusted_http_url;
use crate::provider_auth::OAuthFlow;
use crate::types::{AuthEvent, DeviceCode, ModelAuth, OAuthCredential};

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_MAX_RETRIES: u32 = 3;

struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    interval_seconds: f64,
    expires_in_seconds: f64,
}

struct TokenResponse {
    access: String,
    refresh: String,
    expires: i64,
}

pub struct KimiCodingOAuth {
    http: OAuthHttp,
    oauth_host: Option<String>,
}

impl Default for KimiCodingOAuth {
    fn default() -> Self {
        Self::new(OAuthHttp::default())
    }
}

impl KimiCodingOAuth {
    pub fn new(http: OAuthHttp) -> Self {
        Self {
            http,
            oauth_host: None,
        }
    }

    /// Pin the OAuth host, bypassing the environment overrides.
    pub fn with_oauth_host(mut self, host: impl Into<String>) -> Self {
        self.oauth_host = Some(host.into().trim_end_matches('/').to_string());
        self
    }

    /// `KIMI_CODE_OAUTH_HOST` / `KIMI_OAUTH_HOST`, else the default host.
    fn oauth_host(&self) -> String {
        if let Some(host) = &self.oauth_host {
            return host.clone();
        }
        let override_host = std::env::var("KIMI_CODE_OAUTH_HOST")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                std::env::var("KIMI_OAUTH_HOST")
                    .ok()
                    .filter(|v| !v.is_empty())
            });
        override_host
            .unwrap_or_else(|| DEFAULT_OAUTH_HOST.to_string())
            .trim_end_matches('/')
            .to_string()
    }

    async fn start_device_authorization(
        &self,
        oauth_host: &str,
        signal: &AbortSignal,
    ) -> Result<DeviceAuthorization, AuthError> {
        let response = self
            .http
            .post_form(
                &format!("{oauth_host}/api/oauth/device_authorization"),
                &[("client_id", CLIENT_ID)],
                &[],
                signal,
                Some(REQUEST_TIMEOUT),
            )
            .await?;

        if !response.ok() {
            let suffix = if response.body.is_empty() {
                String::new()
            } else {
                format!(": {}", response.body)
            };
            return Err(AuthError::oauth(format!(
                "Kimi Code device authorization failed with status {}{suffix}",
                response.status
            )));
        }

        let json = response.json_object().unwrap_or_default();
        let device_code = str_field(&json, "device_code");
        let user_code = str_field(&json, "user_code");
        let verification_uri =
            str_field(&json, "verification_uri").and_then(|u| trusted_http_url(&u));
        let verification_uri_complete =
            str_field(&json, "verification_uri_complete").and_then(|u| trusted_http_url(&u));

        let (Some(device_code), Some(user_code), Some(_), Some(verification_uri_complete)) = (
            device_code,
            user_code,
            verification_uri,
            verification_uri_complete,
        ) else {
            return Err(AuthError::oauth(format!(
                "Invalid Kimi Code device authorization response: {}",
                response.body
            )));
        };

        Ok(DeviceAuthorization {
            device_code,
            user_code,
            verification_uri_complete,
            interval_seconds: num_field(&json, "interval")
                .filter(|i| i.is_finite() && *i > 0.0)
                .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
            expires_in_seconds: num_field(&json, "expires_in")
                .filter(|e| e.is_finite() && *e > 0.0)
                .unwrap_or(DEVICE_CODE_TIMEOUT_SECONDS),
        })
    }

    async fn poll_for_token(
        &self,
        oauth_host: &str,
        device: &DeviceAuthorization,
        signal: &AbortSignal,
    ) -> Result<TokenResponse, AuthError> {
        let url = format!("{oauth_host}/api/oauth/token");
        poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: Some(device.interval_seconds),
                expires_in_seconds: Some(device.expires_in_seconds),
                wait_before_first_poll: true,
                signal: signal.clone(),
            },
            || async {
                let response = self
                    .http
                    .post_form(
                        &url,
                        &[
                            ("client_id", CLIENT_ID),
                            ("device_code", &device.device_code),
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ],
                        &[],
                        signal,
                        Some(REQUEST_TIMEOUT),
                    )
                    .await?;

                if response.status >= 500 {
                    let suffix = if response.body.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", response.body)
                    };
                    return Ok(PollResult::Failed {
                        message: format!(
                            "Kimi Code device token request failed with status {}{suffix}",
                            response.status
                        ),
                    });
                }

                let json = response.json_object().unwrap_or_default();
                if response.ok() && json.contains_key("access_token") {
                    return Ok(match parse_token_response(&json, "poll") {
                        Ok(token) => PollResult::Complete(token),
                        Err(error) => PollResult::Failed {
                            message: error.message(),
                        },
                    });
                }

                let error = str_field(&json, "error");
                Ok(match error.as_deref() {
                    Some("authorization_pending") => PollResult::Pending,
                    Some("slow_down") => PollResult::SlowDown {
                        interval_seconds: num_field(&json, "interval").filter(|i| *i > 0.0),
                    },
                    Some("expired_token") => PollResult::Failed {
                        message: "Kimi Code device authorization expired. Please restart login."
                            .into(),
                    },
                    Some("access_denied") => PollResult::Failed {
                        message: "Kimi Code login was denied.".into(),
                    },
                    other => {
                        let detail = other
                            .map(|error| {
                                let description = str_field(&json, "error_description")
                                    .map(|d| format!(": {d}"))
                                    .unwrap_or_default();
                                format!(": {error}{description}")
                            })
                            .unwrap_or_default();
                        PollResult::Failed {
                            message: format!(
                                "Kimi Code device token request failed (status {}){detail}",
                                response.status
                            ),
                        }
                    }
                })
            },
        )
        .await
    }

    async fn refresh_token(
        &self,
        oauth_host: &str,
        refresh_token: &str,
        signal: &AbortSignal,
    ) -> Result<TokenResponse, AuthError> {
        let url = format!("{oauth_host}/api/oauth/token");
        let mut last_error: Option<AuthError> = None;

        for attempt in 0..=REFRESH_MAX_RETRIES {
            if attempt > 0 {
                abortable_sleep(Duration::from_millis(1000 * 2u64.pow(attempt - 1)), signal)
                    .await?;
            }
            if signal.is_aborted() {
                return Err(AuthError::Cancelled);
            }

            let response = match self
                .http
                .post_form(
                    &url,
                    &[
                        ("client_id", CLIENT_ID),
                        ("grant_type", "refresh_token"),
                        ("refresh_token", refresh_token),
                    ],
                    &[],
                    signal,
                    Some(REQUEST_TIMEOUT),
                )
                .await
            {
                Ok(response) => response,
                Err(AuthError::Cancelled) => return Err(AuthError::Cancelled),
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };

            let json = response.json_object().unwrap_or_default();
            if response.ok() {
                return parse_token_response(&json, "refresh");
            }

            // Unauthorized: the stored credential is dead. The resolver
            // surfaces this so the app can clear it and prompt a re-login.
            if response.status == 401
                || response.status == 403
                || str_field(&json, "error").as_deref() == Some("invalid_grant")
            {
                let description = str_field(&json, "error_description")
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                return Err(AuthError::oauth(format!(
                    "Kimi Code token refresh unauthorized (status {}){description}",
                    response.status
                )));
            }

            let retryable = response.status == 429 || response.status >= 500;
            if retryable && attempt < REFRESH_MAX_RETRIES {
                last_error = Some(AuthError::oauth(format!(
                    "Kimi Code token refresh failed with status {}",
                    response.status
                )));
                continue;
            }

            return Err(AuthError::oauth(format!(
                "Kimi Code token refresh failed with status {}: {}",
                response.status,
                Value::Object(json)
            )));
        }

        Err(last_error.unwrap_or_else(|| AuthError::oauth("Kimi Code token refresh failed")))
    }
}

fn parse_token_response(
    json: &Map<String, Value>,
    operation: &str,
) -> Result<TokenResponse, AuthError> {
    let (Some(access), Some(refresh), Some(expires_in)) = (
        str_field(json, "access_token"),
        str_field(json, "refresh_token"),
        num_field(json, "expires_in").filter(|e| e.is_finite() && *e > 0.0),
    ) else {
        return Err(AuthError::oauth(format!(
            "Kimi Code token {operation} response missing fields: {}",
            Value::Object(json.clone())
        )));
    };
    Ok(TokenResponse {
        access,
        refresh,
        expires: now_ms() + (expires_in * 1000.0) as i64,
    })
}

#[async_trait]
impl OAuthFlow for KimiCodingOAuth {
    fn name(&self) -> &str {
        "Kimi Code (subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Kimi Code")
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let oauth_host = self.oauth_host();
        let device = self
            .start_device_authorization(&oauth_host, &ctx.signal)
            .await?;

        ctx.emit(AuthEvent::DeviceCode(DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri_complete.clone(),
            interval_seconds: Some(device.interval_seconds as u64),
            expires_in_seconds: Some(device.expires_in_seconds as u64),
        }));

        let token = self
            .poll_for_token(&oauth_host, &device, &ctx.signal)
            .await?;
        Ok(OAuthCredential::new(
            token.access,
            token.refresh,
            token.expires,
        ))
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let token = self
            .refresh_token(&self.oauth_host(), &credential.refresh, &signal)
            .await?;
        Ok(OAuthCredential::new(
            token.access,
            token.refresh,
            token.expires,
        ))
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::default()
            .with_header("Authorization", format!("Bearer {}", credential.access)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn to_auth_returns_a_bearer_header_not_an_api_key() {
        let flow = KimiCodingOAuth::default();
        let auth = flow
            .to_auth(&OAuthCredential::new("new-access", "r", 0))
            .await
            .unwrap();
        assert_eq!(auth.api_key, None);
        assert_eq!(
            auth.headers.get("Authorization").and_then(Option::as_deref),
            Some("Bearer new-access")
        );
    }

    #[test]
    fn a_pinned_host_loses_its_trailing_slash() {
        let flow = KimiCodingOAuth::default().with_oauth_host("https://auth.example.com/");
        assert_eq!(flow.oauth_host(), "https://auth.example.com");
    }

    #[test]
    fn token_responses_must_carry_all_three_fields() {
        let complete = serde_json::json!({
            "access_token": "a", "refresh_token": "r", "expires_in": 3600
        });
        let token = parse_token_response(complete.as_object().unwrap(), "refresh").unwrap();
        assert_eq!(token.access, "a");
        assert!(token.expires >= now_ms() + 3_600_000 - 1_000);

        for incomplete in [
            serde_json::json!({ "access_token": "a", "refresh_token": "r" }),
            serde_json::json!({ "access_token": "a", "expires_in": 3600 }),
            serde_json::json!({ "access_token": "a", "refresh_token": "r", "expires_in": 0 }),
        ] {
            assert!(parse_token_response(incomplete.as_object().unwrap(), "poll").is_err());
        }
    }
}
