//! Port of `packages/ai/src/auth/oauth/xai.ts` — device-code grant.

use async_trait::async_trait;
use pi_core::options::AbortSignal;
use serde_json::{Map, Value};

use crate::error::AuthError;
use crate::http::{num_field, str_field, OAuthHttp};
use crate::interaction::LoginContext;
use crate::oauth::device_code::{poll_device_code_flow, DeviceCodePollOptions, PollResult};
use crate::oauth::shared::{expires_at, trusted_https_url};
use crate::provider_auth::OAuthFlow;
use crate::types::{AuthEvent, DeviceCode, ModelAuth, OAuthCredential};

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// Refresh slightly before the reported expiry so a token cannot die mid-request.
const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;
const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;

struct XaiDeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval_seconds: Option<f64>,
    expires_in_seconds: f64,
}

pub struct XaiOAuth {
    http: OAuthHttp,
    device_code_url: String,
    token_url: String,
}

impl Default for XaiOAuth {
    fn default() -> Self {
        Self::new(OAuthHttp::default())
    }
}

impl XaiOAuth {
    pub fn new(http: OAuthHttp) -> Self {
        Self {
            http,
            device_code_url: DEVICE_CODE_URL.to_string(),
            token_url: TOKEN_URL.to_string(),
        }
    }

    pub fn with_urls(
        mut self,
        device_code_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        self.device_code_url = device_code_url.into();
        self.token_url = token_url.into();
        self
    }

    async fn post_form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        signal: &AbortSignal,
    ) -> Result<(bool, u16, Map<String, Value>), AuthError> {
        let response = self.http.post_form(url, fields, &[], signal, None).await?;
        let Some(body) = response.json_object() else {
            return Err(AuthError::oauth(format!(
                "xAI OAuth returned invalid JSON (HTTP {})",
                response.status
            )));
        };
        Ok((response.ok(), response.status, body))
    }
}

fn required_string(body: &Map<String, Value>, field: &str) -> Result<String, AuthError> {
    str_field(body, field)
        .ok_or_else(|| AuthError::oauth(format!("Invalid xAI OAuth response field: {field}")))
}

fn positive_number(body: &Map<String, Value>, field: &str) -> Result<f64, AuthError> {
    match body.get(field) {
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.0)
            .ok_or_else(|| AuthError::oauth(format!("Invalid xAI OAuth response field: {field}"))),
        _ => Err(AuthError::oauth(format!(
            "Invalid xAI OAuth response field: {field}"
        ))),
    }
}

/// The verification URI is opened in the user's browser; force https so a
/// malicious response cannot make the host launch something else.
fn validate_verification_uri(raw: &str) -> Result<String, AuthError> {
    trusted_https_url(raw)
        .ok_or_else(|| AuthError::oauth("Untrusted verification URI in xAI OAuth response"))
}

fn request_failure(action: &str, status: u16, body: &Map<String, Value>) -> AuthError {
    let error = str_field(body, "error");
    let description = str_field(body, "error_description");
    let detail = [error, description]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(": ");
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    AuthError::oauth(format!("xAI OAuth {action} failed (HTTP {status}){suffix}"))
}

fn parse_device_code(body: &Map<String, Value>) -> Result<XaiDeviceCode, AuthError> {
    // RFC 8628 allows interval 0 (no minimum wait); fall back to the poller's
    // default instead of failing on a non-positive or malformed value.
    let interval_seconds = num_field(body, "interval").filter(|i| i.is_finite() && *i > 0.0);
    let verification_uri_complete = match str_field(body, "verification_uri_complete") {
        Some(raw) => Some(validate_verification_uri(&raw)?),
        None => None,
    };
    Ok(XaiDeviceCode {
        device_code: required_string(body, "device_code")?,
        user_code: required_string(body, "user_code")?,
        verification_uri: validate_verification_uri(&required_string(body, "verification_uri")?)?,
        verification_uri_complete,
        interval_seconds,
        expires_in_seconds: positive_number(body, "expires_in")?,
    })
}

fn credentials_from_token_response(
    body: &Map<String, Value>,
    previous_refresh_token: Option<&str>,
) -> Result<OAuthCredential, AuthError> {
    let access = required_string(body, "access_token")?;
    // xAI may omit refresh_token on refresh when the token is not rotated.
    let refresh = match (body.get("refresh_token"), previous_refresh_token) {
        (None, Some(previous)) => previous.to_string(),
        _ => required_string(body, "refresh_token")?,
    };
    let expires_in_seconds = if body.get("expires_in").is_none() {
        DEFAULT_TOKEN_LIFETIME_SECONDS
    } else {
        positive_number(body, "expires_in")?
    };
    Ok(OAuthCredential::new(
        access,
        refresh,
        expires_at(expires_in_seconds, REFRESH_SKEW_MS),
    ))
}

#[async_trait]
impl OAuthFlow for XaiOAuth {
    fn name(&self) -> &str {
        "xAI (Grok/X subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with SuperGrok or X Premium")
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let (ok, status, body) = self
            .post_form(
                &self.device_code_url,
                &[
                    ("client_id", CLIENT_ID),
                    ("scope", SCOPE),
                    ("referrer", "pi"),
                ],
                &ctx.signal,
            )
            .await?;
        if !ok {
            return Err(request_failure("device authorization", status, &body));
        }
        let device = parse_device_code(&body)?;

        ctx.emit(AuthEvent::DeviceCode(DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device
                .verification_uri_complete
                .clone()
                .unwrap_or_else(|| device.verification_uri.clone()),
            interval_seconds: device.interval_seconds.map(|i| i as u64),
            expires_in_seconds: Some(device.expires_in_seconds as u64),
        }));

        poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: device.interval_seconds,
                expires_in_seconds: Some(device.expires_in_seconds),
                wait_before_first_poll: true,
                signal: ctx.signal.clone(),
            },
            || async {
                let (ok, status, body) = self
                    .post_form(
                        &self.token_url,
                        &[
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ("client_id", CLIENT_ID),
                            ("device_code", &device.device_code),
                        ],
                        &ctx.signal,
                    )
                    .await?;

                if ok {
                    return Ok(match credentials_from_token_response(&body, None) {
                        Ok(credential) => PollResult::Complete(credential),
                        Err(error) => PollResult::Failed {
                            message: error.message(),
                        },
                    });
                }

                Ok(match str_field(&body, "error").as_deref() {
                    Some("authorization_pending") => PollResult::Pending,
                    Some("slow_down") => PollResult::SlowDown {
                        interval_seconds: num_field(&body, "interval"),
                    },
                    Some("access_denied") | Some("authorization_denied") => PollResult::Failed {
                        message: "xAI device authorization was denied".into(),
                    },
                    Some("expired_token") => PollResult::Failed {
                        message: "xAI device code expired".into(),
                    },
                    _ => PollResult::Failed {
                        message: request_failure("device token polling", status, &body).message(),
                    },
                })
            },
        )
        .await
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let (ok, status, body) = self
            .post_form(
                &self.token_url,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", CLIENT_ID),
                    ("refresh_token", &credential.refresh),
                ],
                &signal,
            )
            .await?;
        if !ok {
            return Err(request_failure("token refresh", status, &body));
        }
        credentials_from_token_response(&body, Some(&credential.refresh))
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::from_api_key(credential.access.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::now_ms;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[tokio::test]
    async fn to_auth_derives_the_api_key_from_the_access_token() {
        let flow = XaiOAuth::default();
        let auth = flow
            .to_auth(&OAuthCredential::new("token", "r", 0))
            .await
            .unwrap();
        assert_eq!(auth, ModelAuth::from_api_key("token"));
    }

    #[test]
    fn a_non_https_verification_uri_is_rejected() {
        let body = object(json!({
            "device_code": "d", "user_code": "u",
            "verification_uri": "http://auth.x.ai/device", "expires_in": 600
        }));
        assert!(parse_device_code(&body).is_err());

        let complete = object(json!({
            "device_code": "d", "user_code": "u",
            "verification_uri": "https://auth.x.ai/device",
            "verification_uri_complete": "javascript:alert(1)",
            "expires_in": 600
        }));
        assert!(parse_device_code(&complete).is_err());
    }

    #[test]
    fn interval_zero_falls_back_to_the_poller_default() {
        let body = object(json!({
            "device_code": "d", "user_code": "u",
            "verification_uri": "https://auth.x.ai/device", "expires_in": 600, "interval": 0
        }));
        assert_eq!(parse_device_code(&body).unwrap().interval_seconds, None);
    }

    #[test]
    fn verification_uri_complete_is_preferred_when_present() {
        let body = object(json!({
            "device_code": "d", "user_code": "u",
            "verification_uri": "https://auth.x.ai/device",
            "verification_uri_complete": "https://auth.x.ai/device?code=u",
            "expires_in": 600
        }));
        let device = parse_device_code(&body).unwrap();
        assert_eq!(
            device.verification_uri_complete.as_deref(),
            Some("https://auth.x.ai/device?code=u")
        );
    }

    #[test]
    fn an_unrotated_refresh_token_is_preserved() {
        let body = object(json!({ "access_token": "new-access", "expires_in": 3600 }));
        let credential = credentials_from_token_response(&body, Some("old-refresh")).unwrap();
        assert_eq!(credential.access, "new-access");
        assert_eq!(credential.refresh, "old-refresh");

        // Without a previous token there is nothing to fall back to.
        assert!(credentials_from_token_response(&body, None).is_err());
    }

    #[test]
    fn a_missing_expires_in_assumes_a_one_hour_lifetime() {
        let body = object(json!({ "access_token": "a", "refresh_token": "r" }));
        let credential = credentials_from_token_response(&body, None).unwrap();
        let expected = now_ms() + 3_600_000 - REFRESH_SKEW_MS;
        assert!((credential.expires - expected).abs() < 1_000);
    }

    #[test]
    fn token_responses_with_missing_fields_are_rejected() {
        let body = object(json!({ "refresh_token": "r", "expires_in": 3600 }));
        assert!(credentials_from_token_response(&body, None).is_err());
    }

    #[test]
    fn failures_surface_the_upstream_code_and_description() {
        let body = object(json!({ "error": "invalid_grant", "error_description": "expired" }));
        let error = request_failure("token refresh", 400, &body);
        assert_eq!(
            error.message(),
            "oauth error: xAI OAuth token refresh failed (HTTP 400): invalid_grant: expired"
        );
    }
}
