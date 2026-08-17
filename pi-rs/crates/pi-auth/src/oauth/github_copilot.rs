//! Port of `packages/ai/src/auth/oauth/github-copilot.ts` — device-code login
//! against GitHub, then a Copilot token exchange.

use std::time::Duration;

use async_trait::async_trait;
use pi_core::message::now_ms;
use pi_core::options::AbortSignal;
use serde_json::{json, Value};

use crate::error::AuthError;
use crate::http::{num_field, str_field, OAuthHttp};
use crate::interaction::LoginContext;
use crate::oauth::device_code::{
    abortable_sleep, poll_device_code_flow, DeviceCodePollOptions, PollResult,
};
use crate::oauth::shared::trusted_http_url;
use crate::provider_auth::OAuthFlow;
use crate::types::{AuthEvent, DeviceCode, ModelAuth, OAuthCredential, TextPrompt};

/// Base64 of the client id, as upstream stores it.
const CLIENT_ID_B64: &str = "SXYxLmI1MDdhMDhjODdlY2ZlOTg=";
const COPILOT_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "GitHubCopilotChat/0.35.0"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
    ("Copilot-Integration-Id", "vscode-chat"),
];
const COPILOT_API_VERSION: &str = "2026-06-01";
const MAX_RETRY_AFTER: Duration = Duration::from_millis(10_000);
const DEFAULT_RETRY_AFTER: Duration = Duration::from_millis(1_000);
const INDIVIDUAL_BASE_URL: &str = "https://api.individual.githubcopilot.com";
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

/// `hostname` of a domain or URL, or `None` when it is neither.
fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    url::Url::parse(&candidate)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
}

/// `proxy-ep=proxy.xxx` in the Copilot token names the account's API host.
fn base_url_from_token(token: &str) -> Option<String> {
    let proxy_host = token
        .split(';')
        .find_map(|part| part.trim().strip_prefix("proxy-ep="))?;
    if proxy_host.is_empty() {
        return None;
    }
    let api_host = proxy_host
        .strip_prefix("proxy.")
        .map(|rest| format!("api.{rest}"))
        .unwrap_or_else(|| proxy_host.to_string());
    Some(format!("https://{api_host}"))
}

struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Option<f64>,
    expires_in: f64,
}

pub struct GitHubCopilotOAuth {
    http: OAuthHttp,
    /// Replaces the `https://{domain}` derivation wholesale. Tests point this
    /// at a mock server; nothing in production sets it.
    endpoint_override: Option<String>,
    /// Model ids whose Copilot policy is accepted after login. Upstream reads
    /// these from its bundled `GITHUB_COPILOT_MODELS` catalog, which lives in
    /// another crate here, so the host injects them.
    policy_model_ids: Vec<String>,
}

impl Default for GitHubCopilotOAuth {
    fn default() -> Self {
        Self::new(OAuthHttp::default())
    }
}

impl GitHubCopilotOAuth {
    pub fn new(http: OAuthHttp) -> Self {
        Self {
            http,
            endpoint_override: None,
            policy_model_ids: Vec::new(),
        }
    }

    /// Override every GitHub/Copilot endpoint with one base URL.
    pub fn with_endpoint_override(mut self, base: impl Into<String>) -> Self {
        self.endpoint_override = Some(base.into());
        self
    }

    /// Models whose policy login should enable (see `policy_model_ids`).
    pub fn with_policy_model_ids(
        mut self,
        ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.policy_model_ids = ids.into_iter().map(Into::into).collect();
        self
    }

    fn device_code_url(&self, domain: &str) -> String {
        match &self.endpoint_override {
            Some(base) => format!("{base}/login/device/code"),
            None => format!("https://{domain}/login/device/code"),
        }
    }

    fn access_token_url(&self, domain: &str) -> String {
        match &self.endpoint_override {
            Some(base) => format!("{base}/login/oauth/access_token"),
            None => format!("https://{domain}/login/oauth/access_token"),
        }
    }

    fn copilot_token_url(&self, domain: &str) -> String {
        match &self.endpoint_override {
            Some(base) => format!("{base}/copilot_internal/v2/token"),
            None => format!("https://api.{domain}/copilot_internal/v2/token"),
        }
    }

    fn api_base_url(&self, token: &str, enterprise_domain: Option<&str>) -> String {
        if let Some(base) = &self.endpoint_override {
            return base.clone();
        }
        Self::derive_base_url(token, enterprise_domain)
    }

    /// The pure part of the base-URL derivation, shared with `to_auth`.
    fn derive_base_url(token: &str, enterprise_domain: Option<&str>) -> String {
        if let Some(url) = base_url_from_token(token) {
            return url;
        }
        match enterprise_domain {
            Some(domain) => format!("https://copilot-api.{domain}"),
            None => INDIVIDUAL_BASE_URL.to_string(),
        }
    }

    async fn start_device_flow(
        &self,
        domain: &str,
        signal: &AbortSignal,
    ) -> Result<DeviceAuthorization, AuthError> {
        let response = self
            .http
            .post_form(
                &self.device_code_url(domain),
                &[("client_id", &client_id()), ("scope", "read:user")],
                &[("User-Agent", "GitHubCopilotChat/0.35.0")],
                signal,
                None,
            )
            .await?;
        if !response.ok() {
            return Err(AuthError::oauth(format!(
                "{}: {}",
                response.status, response.body
            )));
        }
        let json = response
            .json_object()
            .ok_or_else(|| AuthError::oauth("Invalid device code response"))?;

        let (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in)) = (
            str_field(&json, "device_code"),
            str_field(&json, "user_code"),
            str_field(&json, "verification_uri"),
            num_field(&json, "expires_in"),
        ) else {
            return Err(AuthError::oauth("Invalid device code response fields"));
        };

        // The verification URI is opened in the user's browser, so force it to
        // be an http(s) URL rather than something `open` would execute.
        let verification_uri = trusted_http_url(&verification_uri).ok_or_else(|| {
            AuthError::oauth("Untrusted verification_uri in device code response")
        })?;

        Ok(DeviceAuthorization {
            device_code,
            user_code,
            verification_uri,
            interval: num_field(&json, "interval"),
            expires_in,
        })
    }

    async fn poll_for_access_token(
        &self,
        domain: &str,
        device: &DeviceAuthorization,
        signal: &AbortSignal,
    ) -> Result<String, AuthError> {
        let url = self.access_token_url(domain);
        poll_device_code_flow(
            DeviceCodePollOptions {
                interval_seconds: device.interval,
                expires_in_seconds: Some(device.expires_in),
                wait_before_first_poll: true,
                signal: signal.clone(),
            },
            || async {
                let response = self
                    .http
                    .post_form(
                        &url,
                        &[
                            ("client_id", &client_id()),
                            ("device_code", &device.device_code),
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ],
                        &[("User-Agent", "GitHubCopilotChat/0.35.0")],
                        signal,
                        None,
                    )
                    .await?;

                // GitHub answers 200 with an `error` body for pending states.
                if !response.ok() && response.json_object().is_none() {
                    return Err(AuthError::oauth(format!(
                        "{}: {}",
                        response.status, response.body
                    )));
                }

                let Some(json) = response.json_object() else {
                    return Ok(PollResult::Failed {
                        message: "Invalid device token response".into(),
                    });
                };

                if let Some(access_token) = str_field(&json, "access_token") {
                    return Ok(PollResult::Complete(access_token));
                }

                let Some(error) = str_field(&json, "error") else {
                    return Ok(PollResult::Failed {
                        message: "Invalid device token response".into(),
                    });
                };

                Ok(match error.as_str() {
                    "authorization_pending" => PollResult::Pending,
                    "slow_down" => PollResult::SlowDown {
                        interval_seconds: num_field(&json, "interval"),
                    },
                    _ => {
                        let description = str_field(&json, "error_description")
                            .map(|d| format!(": {d}"))
                            .unwrap_or_default();
                        PollResult::Failed {
                            message: format!("Device flow failed: {error}{description}"),
                        }
                    }
                })
            },
        )
        .await
    }

    /// Exchange the long-lived GitHub token for a short-lived Copilot token.
    async fn exchange_copilot_token(
        &self,
        github_token: &str,
        enterprise_domain: Option<&str>,
        signal: &AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let domain = enterprise_domain.unwrap_or("github.com");
        let authorization = format!("Bearer {github_token}");
        let mut headers: Vec<(&str, &str)> = vec![("Authorization", &authorization)];
        headers.extend_from_slice(COPILOT_HEADERS);

        let response = self
            .http
            .get(&self.copilot_token_url(domain), &headers, signal, None)
            .await?;
        if !response.ok() {
            return Err(AuthError::oauth(format!(
                "{}: {}",
                response.status, response.body
            )));
        }
        let json = response
            .json_object()
            .ok_or_else(|| AuthError::oauth("Invalid Copilot token response"))?;

        let (Some(token), Some(expires_at)) =
            (str_field(&json, "token"), num_field(&json, "expires_at"))
        else {
            return Err(AuthError::oauth("Invalid Copilot token response fields"));
        };

        let mut credential = OAuthCredential::new(
            token,
            github_token.to_string(),
            (expires_at * 1000.0) as i64 - EXPIRY_SKEW_MS,
        );
        if let Some(domain) = enterprise_domain {
            credential = credential.with_extra("enterpriseUrl", Value::String(domain.to_string()));
        }
        Ok(credential)
    }

    async fn fetch_available_model_ids(
        &self,
        copilot_token: &str,
        enterprise_domain: Option<&str>,
        signal: &AbortSignal,
    ) -> Result<Vec<String>, AuthError> {
        let base_url = self.api_base_url(copilot_token, enterprise_domain);
        // Some Individual accounts report false for every picker flag despite
        // explicit enabled policies; limit the fallback to that endpoint.
        let allow_policy_fallback = base_url == INDIVIDUAL_BASE_URL;
        let url = format!("{base_url}/models");
        let authorization = format!("Bearer {copilot_token}");
        let mut headers: Vec<(&str, &str)> = vec![
            ("Authorization", &authorization),
            ("X-GitHub-Api-Version", COPILOT_API_VERSION),
        ];
        headers.extend_from_slice(COPILOT_HEADERS);

        let request = || {
            self.http
                .get(&url, &headers, signal, Some(Duration::from_secs(5)))
        };

        // Login-time policy updates can drain the Copilot rate-limit bucket;
        // honor Retry-After and retry once instead of failing the login.
        let mut response = request().await?;
        if response.status == 429 {
            let wait = response
                .header("retry-after")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| Duration::from_millis((s * 1000.0) as u64).min(MAX_RETRY_AFTER))
                .unwrap_or(DEFAULT_RETRY_AFTER);
            abortable_sleep(wait, signal).await?;
            response = request().await?;
        }
        if !response.ok() {
            return Err(AuthError::oauth(format!(
                "{}: {}",
                response.status, response.body
            )));
        }

        parse_available_model_ids(&response.body, allow_policy_fallback)
    }

    /// Accept the model policies that gate Claude/Grok on a Copilot account.
    /// Failures are ignored, matching upstream: a policy that cannot be set
    /// should not fail an otherwise good login.
    async fn enable_model_policies(
        &self,
        token: &str,
        enterprise_domain: Option<&str>,
        signal: &AbortSignal,
    ) -> Result<(), AuthError> {
        let base_url = self.api_base_url(token, enterprise_domain);
        let authorization = format!("Bearer {token}");
        for model_id in &self.policy_model_ids {
            if signal.is_aborted() {
                return Err(AuthError::Cancelled);
            }
            let url = format!("{base_url}/models/{model_id}/policy");
            let mut headers: Vec<(&str, &str)> = vec![
                ("Authorization", &authorization),
                ("openai-intent", "chat-policy"),
                ("x-interaction-type", "chat-policy"),
            ];
            headers.extend_from_slice(COPILOT_HEADERS);
            let result = self
                .http
                .post_json(&url, &json!({ "state": "enabled" }), &headers, signal, None)
                .await;
            if let Err(AuthError::Cancelled) = result {
                return Err(AuthError::Cancelled);
            }
        }
        Ok(())
    }
}

fn parse_available_model_ids(
    body: &str,
    allow_policy_fallback: bool,
) -> Result<Vec<String>, AuthError> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|_| AuthError::oauth("Invalid Copilot models response"))?;
    let Some(data) = parsed.get("data").and_then(Value::as_array) else {
        return Err(AuthError::oauth("Invalid Copilot models response"));
    };

    let mut picker_ids = Vec::new();
    let mut policy_enabled_ids = Vec::new();
    for item in data {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let supports_tool_calls = item
            .get("capabilities")
            .and_then(|c| c.get("supports"))
            .and_then(|s| s.get("tool_calls"))
            .and_then(Value::as_bool);
        if supports_tool_calls == Some(false) {
            continue;
        }
        let policy_state = item
            .get("policy")
            .and_then(|p| p.get("state"))
            .and_then(Value::as_str);
        if item.get("model_picker_enabled").and_then(Value::as_bool) == Some(true)
            && policy_state != Some("disabled")
        {
            picker_ids.push(id.to_string());
        }
        if policy_state == Some("enabled") {
            policy_enabled_ids.push(id.to_string());
        }
    }

    Ok(if !picker_ids.is_empty() || !allow_policy_fallback {
        picker_ids
    } else {
        policy_enabled_ids
    })
}

fn credential_enterprise_domain(credential: &OAuthCredential) -> Option<String> {
    credential
        .extra_str("enterpriseUrl")
        .filter(|v| !v.is_empty())
        .and_then(normalize_domain)
}

#[async_trait]
impl OAuthFlow for GitHubCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(&self, ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let input = ctx
            .interaction
            .prompt_text(
                TextPrompt::new("GitHub Enterprise URL/domain (blank for github.com)")
                    .placeholder("company.ghe.com"),
            )
            .await?;
        ctx.throw_if_aborted()?;

        let trimmed = input.trim();
        let enterprise_domain = normalize_domain(&input);
        if !trimmed.is_empty() && enterprise_domain.is_none() {
            return Err(AuthError::oauth("Invalid GitHub Enterprise URL/domain"));
        }
        let domain = enterprise_domain
            .clone()
            .unwrap_or_else(|| "github.com".into());

        let device = self.start_device_flow(&domain, &ctx.signal).await?;
        ctx.emit(AuthEvent::DeviceCode(DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            interval_seconds: device.interval.map(|i| i as u64),
            expires_in_seconds: Some(device.expires_in as u64),
        }));

        let github_token = self
            .poll_for_access_token(&domain, &device, &ctx.signal)
            .await?;
        let credential = self
            .exchange_copilot_token(&github_token, enterprise_domain.as_deref(), &ctx.signal)
            .await?;

        ctx.progress("Enabling models...");
        self.enable_model_policies(
            &credential.access,
            enterprise_domain.as_deref(),
            &ctx.signal,
        )
        .await?;

        let model_ids = self
            .fetch_available_model_ids(
                &credential.access,
                enterprise_domain.as_deref(),
                &ctx.signal,
            )
            .await?;
        Ok(credential.with_extra("availableModelIds", json!(model_ids)))
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let enterprise_domain = credential_enterprise_domain(credential);
        let refreshed = self
            .exchange_copilot_token(&credential.refresh, enterprise_domain.as_deref(), &signal)
            .await?;
        let model_ids = self
            .fetch_available_model_ids(&refreshed.access, enterprise_domain.as_deref(), &signal)
            .await?;
        Ok(refreshed.with_extra("availableModelIds", json!(model_ids)))
    }

    /// Derive the credential-specific proxy endpoint for each request.
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        let enterprise_domain = credential_enterprise_domain(credential);
        Ok(
            ModelAuth::from_api_key(credential.access.clone()).with_base_url(
                Self::derive_base_url(&credential.access, enterprise_domain.as_deref()),
            ),
        )
    }
}

/// Whether a Copilot credential is still usable, for status UI.
pub fn copilot_credential_is_live(credential: &OAuthCredential) -> bool {
    credential.expires > now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_id_decodes() {
        assert_eq!(client_id(), "Iv1.b507a08c87ecfe98");
    }

    #[tokio::test]
    async fn to_auth_derives_base_url_from_the_token_proxy_endpoint() {
        let flow = GitHubCopilotOAuth::default();
        let access = "tid=abc;exp=123;proxy-ep=proxy.enterprise.example;rest";
        let auth = flow
            .to_auth(&OAuthCredential::new(access, "r", 0))
            .await
            .unwrap();
        assert_eq!(auth.api_key.as_deref(), Some(access));
        assert_eq!(
            auth.base_url.as_deref(),
            Some("https://api.enterprise.example")
        );
    }

    #[tokio::test]
    async fn to_auth_falls_back_to_the_enterprise_domain_then_the_individual_endpoint() {
        let flow = GitHubCopilotOAuth::default();

        let enterprise = flow
            .to_auth(
                &OAuthCredential::new("no-proxy-ep", "r", 0)
                    .with_extra("enterpriseUrl", json!("https://company.ghe.com")),
            )
            .await
            .unwrap();
        assert_eq!(
            enterprise.base_url.as_deref(),
            Some("https://copilot-api.company.ghe.com")
        );

        let individual = flow
            .to_auth(&OAuthCredential::new("no-proxy-ep", "r", 0))
            .await
            .unwrap();
        assert_eq!(individual.base_url.as_deref(), Some(INDIVIDUAL_BASE_URL));
    }

    #[test]
    fn domain_normalization_accepts_bare_domains_and_urls() {
        assert_eq!(
            normalize_domain("company.ghe.com").as_deref(),
            Some("company.ghe.com")
        );
        assert_eq!(
            normalize_domain("https://company.ghe.com/path").as_deref(),
            Some("company.ghe.com")
        );
        assert_eq!(normalize_domain("   "), None);
    }

    #[test]
    fn model_filtering_uses_the_picker_catalog() {
        let body = json!({
            "data": [
                { "id": "picked", "model_picker_enabled": true },
                { "id": "not-picked", "model_picker_enabled": false, "policy": { "state": "enabled" } },
                { "id": "disabled-policy", "model_picker_enabled": true, "policy": { "state": "disabled" } },
                { "id": "no-tools", "model_picker_enabled": true,
                  "capabilities": { "supports": { "tool_calls": false } } },
            ]
        })
        .to_string();
        assert_eq!(
            parse_available_model_ids(&body, true).unwrap(),
            vec!["picked".to_string()]
        );
    }

    #[test]
    fn an_empty_picker_catalog_falls_back_to_policy_models_only_for_individual() {
        let body = json!({
            "data": [{ "id": "enabled-by-policy", "model_picker_enabled": false, "policy": { "state": "enabled" } }]
        })
        .to_string();
        assert_eq!(
            parse_available_model_ids(&body, true).unwrap(),
            vec!["enabled-by-policy".to_string()]
        );
        assert!(parse_available_model_ids(&body, false).unwrap().is_empty());
    }

    #[test]
    fn a_malformed_models_response_is_an_error() {
        assert!(parse_available_model_ids("{}", true).is_err());
        assert!(parse_available_model_ids("not json", true).is_err());
    }
}
