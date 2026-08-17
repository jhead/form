//! Port of `packages/ai/src/auth/oauth/radius.ts`.
//!
//! Radius is a pi-messages gateway: the OAuth client APIs live on the
//! configured gateway, and only the interactive browser authorization endpoint
//! is discovered. Model-catalog loading belongs to the Radius provider crate.

use async_trait::async_trait;
use pi_core::options::AbortSignal;
use serde_json::Value;

use crate::error::AuthError;
use crate::http::{num_field, str_field, OAuthHttp};
use crate::interaction::LoginContext;
use crate::oauth::callback_server::{CallbackPage, LoopbackCallback};
use crate::oauth::device_code::{poll_device_code_flow, DeviceCodePollOptions, PollResult};
use crate::oauth::pkce::generate_pkce;
use crate::oauth::shared::expires_at;
use crate::provider_auth::OAuthFlow;
use crate::types::{AuthEvent, DeviceCode, ModelAuth, OAuthCredential, SelectOption, SelectPrompt};

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PORT: u16 = 1456;
const CALLBACK_PATH: &str = "/oauth/callback";
const TOKEN_EXPIRY_SKEW_MS: i64 = 60_000;
const LOGIN_METHOD_BROWSER: &str = "browser";
const LOGIN_METHOD_DEVICE_CODE: &str = "device-code";
const OAUTH_CLIENT_ID: &str = "pi-gateway";
const OAUTH_SCOPE: &str = "gateway offline_access";
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Port of `normalizeRadiusGatewayUrl`: add a scheme, drop trailing slashes.
pub fn normalize_radius_gateway_url(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// An OAuth error response, kept structured so device polling can branch on
/// `error` instead of matching on a message.
#[derive(Debug, Clone)]
struct OAuthResponseError {
    oauth_error: Option<String>,
    message: String,
}

impl OAuthResponseError {
    fn into_auth_error(self) -> AuthError {
        AuthError::oauth(self.message)
    }
}

pub struct RadiusOAuth {
    http: OAuthHttp,
    name: String,
    gateway: String,
}

impl RadiusOAuth {
    /// `createRadiusOAuth({ name, gateway })`.
    pub fn new(http: OAuthHttp, name: impl Into<String>, gateway: impl AsRef<str>) -> Self {
        Self {
            http,
            name: name.into(),
            gateway: normalize_radius_gateway_url(gateway.as_ref()),
        }
    }

    pub fn gateway(&self) -> &str {
        &self.gateway
    }

    async fn discover_authorization_endpoint(
        &self,
        signal: &AbortSignal,
    ) -> Result<String, AuthError> {
        let url = format!("{}/v1/oauth", self.gateway);
        let response = self.http.get(&url, &[], signal, None).await?;
        if !response.ok() {
            return Err(AuthError::oauth(format!(
                "Could not load Radius OAuth config from {}: {} {}",
                self.gateway, response.status, response.body
            )));
        }
        let endpoint = response
            .json_object()
            .and_then(|json| str_field(&json, "authorizationEndpoint"));
        endpoint.ok_or_else(|| {
            AuthError::oauth(format!("Invalid Radius OAuth config from {}", self.gateway))
        })
    }

    async fn request_token(
        &self,
        fields: &[(&str, &str)],
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, OAuthResponseError> {
        let url = format!("{}/v1/oauth/token", self.gateway);
        let response = match self.http.post_form(&url, fields, &[], signal, None).await {
            Ok(response) => response,
            Err(AuthError::Cancelled) => {
                return Err(OAuthResponseError {
                    oauth_error: None,
                    message: "Login cancelled".into(),
                })
            }
            Err(error) => {
                return Err(OAuthResponseError {
                    oauth_error: None,
                    message: error.message(),
                })
            }
        };

        if !response.ok() {
            return Err(read_oauth_response_error(
                response.status,
                &response.body,
                "Radius OAuth token request failed",
            ));
        }

        let json = response.json_object().unwrap_or_default();
        let (Some(access), Some(refresh), Some(expires_in)) = (
            str_field(&json, "access_token"),
            str_field(&json, "refresh_token"),
            num_field(&json, "expires_in"),
        ) else {
            return Err(OAuthResponseError {
                oauth_error: None,
                message: format!(
                    "Radius OAuth token response is missing required fields: {}",
                    response.body
                ),
            });
        };

        let mut credential = OAuthCredential::new(
            access,
            refresh,
            expires_at(expires_in, TOKEN_EXPIRY_SKEW_MS),
        );
        if let Some(scope) = str_field(&json, "scope") {
            credential = credential.with_extra("scope", Value::String(scope));
        }
        Ok(credential)
    }

    async fn login_with_browser(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        let authorization_endpoint = self.discover_authorization_endpoint(&ctx.signal).await?;
        let pkce = generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();
        let redirect_uri = format!("http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}");

        let mut callback = if ctx.local_callback_server {
            Some(
                LoopbackCallback::bind(
                    CALLBACK_HOST,
                    CALLBACK_PORT,
                    CALLBACK_PATH,
                    CallbackPage {
                        success_message: "Signed in to Radius. You may now close this page.".into(),
                        expected_state: Some(state.clone()),
                    },
                )
                .await?,
            )
        } else {
            None
        };

        let mut authorize_url = url::Url::parse(&authorization_endpoint)
            .map_err(|e| AuthError::oauth(format!("invalid Radius authorization endpoint: {e}")))?;
        authorize_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", OAUTH_CLIENT_ID)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", OAUTH_SCOPE)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("handoff", "url")
            .append_pair("state", &state);

        ctx.progress(format!("Listening for OAuth callback on {redirect_uri}"));
        ctx.emit(AuthEvent::AuthUrl {
            url: authorize_url.to_string(),
            instructions: Some("Continue in your browser.".into()),
        });

        // Upstream's browser login has no manual paste-back path, so without a
        // callback server there is nothing to wait on.
        let Some(callback) = callback.as_mut() else {
            return Err(AuthError::interaction(
                "Radius browser login needs a local callback server; enable LoginContext::local_callback_server or choose device-code login",
            ));
        };

        let params = callback.wait(&ctx.signal).await?;
        let code = params
            .as_ref()
            .and_then(|p| p.get("code"))
            .cloned()
            .ok_or_else(|| AuthError::oauth("OAuth callback did not complete."))?;

        self.request_token(
            &[
                ("grant_type", "authorization_code"),
                ("client_id", OAUTH_CLIENT_ID),
                ("redirect_uri", &redirect_uri),
                ("code", &code),
                ("code_verifier", &pkce.verifier),
            ],
            &ctx.signal,
        )
        .await
        .map_err(OAuthResponseError::into_auth_error)
    }

    async fn login_with_device_code(
        &self,
        ctx: &LoginContext,
    ) -> Result<OAuthCredential, AuthError> {
        let url = format!("{}/v1/oauth/device", self.gateway);
        let response = self
            .http
            .post_form(
                &url,
                &[("client_id", OAUTH_CLIENT_ID), ("scope", OAUTH_SCOPE)],
                &[],
                &ctx.signal,
                None,
            )
            .await?;
        if !response.ok() {
            return Err(read_oauth_response_error(
                response.status,
                &response.body,
                "Radius OAuth device authorization failed",
            )
            .into_auth_error());
        }

        let json = response.json_object().unwrap_or_default();
        let (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in)) = (
            str_field(&json, "device_code"),
            str_field(&json, "user_code"),
            str_field(&json, "verification_uri"),
            num_field(&json, "expires_in").filter(|e| *e > 0.0),
        ) else {
            return Err(AuthError::oauth(
                "Radius OAuth device authorization response is missing required fields",
            ));
        };
        let interval = num_field(&json, "interval");

        ctx.emit(AuthEvent::DeviceCode(DeviceCode {
            user_code,
            verification_uri,
            interval_seconds: interval.map(|i| i as u64),
            expires_in_seconds: Some(expires_in as u64),
        }));

        poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: interval,
                expires_in_seconds: Some(expires_in),
                wait_before_first_poll: false,
                signal: ctx.signal.clone(),
            },
            || async {
                match self
                    .request_token(
                        &[
                            ("grant_type", DEVICE_CODE_GRANT_TYPE),
                            ("client_id", OAUTH_CLIENT_ID),
                            ("device_code", &device_code),
                        ],
                        &ctx.signal,
                    )
                    .await
                {
                    Ok(credential) => Ok(PollResult::Complete(credential)),
                    Err(error) => match error.oauth_error.as_deref() {
                        Some("authorization_pending") => Ok(PollResult::Pending),
                        Some("slow_down") => Ok(PollResult::SlowDown {
                            interval_seconds: None,
                        }),
                        Some("expired_token") => Ok(PollResult::Failed {
                            message: "Device authorization expired.".into(),
                        }),
                        Some("access_denied") => Ok(PollResult::Failed {
                            message: "Device authorization was denied.".into(),
                        }),
                        _ => Err(error.into_auth_error()),
                    },
                }
            },
        )
        .await
    }
}

fn read_oauth_response_error(status: u16, body: &str, message: &str) -> OAuthResponseError {
    let mut oauth_error = None;
    let mut description = None;

    if !body.is_empty() {
        match serde_json::from_str::<Value>(body) {
            Ok(Value::Object(data)) => {
                oauth_error = str_field(&data, "error");
                description = str_field(&data, "error_description");
            }
            _ => description = Some(body.to_string()),
        }
    }

    let detail = match (&oauth_error, &description) {
        (Some(error), Some(description)) => format!("{error}: {description}"),
        (Some(error), None) => error.clone(),
        (None, Some(description)) => description.clone(),
        (None, None) => status.to_string(),
    };

    OAuthResponseError {
        oauth_error,
        message: format!("{message}: {detail}"),
    }
}

#[async_trait]
impl OAuthFlow for RadiusOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let method = ctx
            .interaction
            .prompt_select(SelectPrompt {
                message: format!("Sign in to {}:", self.name),
                options: vec![
                    SelectOption::new(LOGIN_METHOD_BROWSER, "Sign in with browser (recommended)"),
                    SelectOption::new(
                        LOGIN_METHOD_DEVICE_CODE,
                        "Sign in with device code (when signing in from another device)",
                    ),
                ],
            })
            .await?;

        match method.as_str() {
            LOGIN_METHOD_DEVICE_CODE => self.login_with_device_code(ctx).await,
            LOGIN_METHOD_BROWSER => self.login_with_browser(ctx).await,
            other => Err(AuthError::oauth(format!(
                "Unknown {} sign-in method: {other}",
                self.name
            ))),
        }
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        self.request_token(
            &[
                ("grant_type", "refresh_token"),
                ("client_id", OAUTH_CLIENT_ID),
                ("refresh_token", &credential.refresh),
            ],
            &signal,
        )
        .await
        .map_err(OAuthResponseError::into_auth_error)
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::from_api_key(credential.access.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_urls_get_a_scheme_and_lose_trailing_slashes() {
        assert_eq!(
            normalize_radius_gateway_url("gateway.example.com/"),
            "https://gateway.example.com"
        );
        assert_eq!(
            normalize_radius_gateway_url("http://localhost:8080///"),
            "http://localhost:8080"
        );
        assert_eq!(
            normalize_radius_gateway_url("https://gateway.example.com"),
            "https://gateway.example.com"
        );
    }

    #[test]
    fn oauth_error_bodies_are_parsed_into_a_branchable_code() {
        let error = read_oauth_response_error(
            400,
            r#"{"error":"authorization_pending","error_description":"waiting"}"#,
            "Radius OAuth token request failed",
        );
        assert_eq!(error.oauth_error.as_deref(), Some("authorization_pending"));
        assert!(error.message.contains("authorization_pending: waiting"));

        let plain =
            read_oauth_response_error(502, "bad gateway", "Radius OAuth token request failed");
        assert_eq!(plain.oauth_error, None);
        assert!(plain.message.ends_with("bad gateway"));

        let empty = read_oauth_response_error(500, "", "Radius OAuth token request failed");
        assert!(empty.message.ends_with("500"));
    }

    #[tokio::test]
    async fn to_auth_derives_the_api_key_from_the_access_token() {
        let flow = RadiusOAuth::new(OAuthHttp::default(), "Radius", "gateway.example.com");
        let auth = flow
            .to_auth(&OAuthCredential::new("token", "r", 0))
            .await
            .unwrap();
        assert_eq!(auth, ModelAuth::from_api_key("token"));
    }
}
