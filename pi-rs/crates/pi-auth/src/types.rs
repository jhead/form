//! Port of `packages/ai/src/auth/types.ts`.
//!
//! The wire shapes here are load-bearing: `Credential` is what lands in
//! `auth.json`, which the TypeScript implementation reads and writes on the
//! same machine. Field names and the `type` tag must match exactly.

use std::collections::BTreeMap;

use pi_core::options::{ProviderEnv, ProviderHeaders};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Auth for a single model request. If a value cannot be expressed as
/// `api_key`, `headers` or `base_url`, it is provider config, not auth.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: ProviderHeaders,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl ModelAuth {
    pub fn from_api_key(key: impl Into<String>) -> Self {
        Self {
            api_key: Some(key.into()),
            ..Default::default()
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), Some(value.into()));
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}

/// Stored api-key credential. `env` holds provider-scoped environment/config
/// values such as Cloudflare account/gateway ids.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: ProviderEnv,
}

impl ApiKeyCredential {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: Some(key.into()),
            env: ProviderEnv::new(),
        }
    }
}

/// Stored canonical OAuth credential.
///
/// Upstream's `OAuthCredentials` is an open interface (`[key: string]: unknown`)
/// and flows stash provider-specific keys on it — `enterpriseUrl` (Copilot),
/// `accountId` (Codex), `scope` (Radius), `availableModelIds`. Those ride in
/// [`OAuthCredential::extra`] so a round-trip through this type is lossless.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub refresh: String,
    pub access: String,
    /// Absolute expiry in epoch milliseconds.
    pub expires: i64,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl OAuthCredential {
    pub fn new(access: impl Into<String>, refresh: impl Into<String>, expires: i64) -> Self {
        Self {
            refresh: refresh.into(),
            access: access.into(),
            expires,
            extra: Map::new(),
        }
    }

    /// Read a provider-specific field, e.g. `"enterpriseUrl"`.
    pub fn extra_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(Value::as_str)
    }

    pub fn with_extra(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extra.insert(key.into(), value);
        self
    }
}

/// One type-tagged credential per provider — the shape of today's `auth.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

impl Credential {
    pub fn credential_type(&self) -> AuthType {
        match self {
            Credential::ApiKey(_) => AuthType::ApiKey,
            Credential::OAuth(_) => AuthType::OAuth,
        }
    }

    pub fn as_api_key(&self) -> Option<&ApiKeyCredential> {
        match self {
            Credential::ApiKey(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_oauth(&self) -> Option<&OAuthCredential> {
        match self {
            Credential::OAuth(c) => Some(c),
            _ => None,
        }
    }
}

impl From<ApiKeyCredential> for Credential {
    fn from(value: ApiKeyCredential) -> Self {
        Credential::ApiKey(value)
    }
}

impl From<OAuthCredential> for Credential {
    fn from(value: OAuthCredential) -> Self {
        Credential::OAuth(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    ApiKey,
    OAuth,
}

impl AuthType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthType::ApiKey => "api_key",
            AuthType::OAuth => "oauth",
        }
    }
}

/// Non-secret credential metadata for account/status enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    pub provider_id: String,
    #[serde(rename = "type")]
    pub credential_type: AuthType,
}

/// Result of resolving auth for a provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResult {
    pub auth: ModelAuth,
    /// Provider-scoped environment/config values resolved from the credential.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: ProviderEnv,
    /// Human-readable label for status UI: `"ANTHROPIC_API_KEY"`, `"OAuth"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Side-effect-free availability report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCheck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "type")]
    pub auth_type: AuthType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInfoLink {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A device-authorization grant in progress, handed to the host UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<u64>,
}

/// Progress events emitted during a login. Port of upstream `AuthEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthEvent {
    Info {
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        links: Vec<AuthInfoLink>,
    },
    AuthUrl {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    DeviceCode(DeviceCode),
    Progress {
        message: String,
    },
}

/// A free-text / secret / manual-code prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPrompt {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

impl TextPrompt {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            placeholder: None,
        }
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl SelectOption {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectPrompt {
    pub message: String,
    pub options: Vec<SelectOption>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_credential_matches_the_typescript_wire_shape() {
        let json = r#"{"type":"api_key","key":"sk-test"}"#;
        let credential: Credential = serde_json::from_str(json).unwrap();
        assert_eq!(
            credential,
            Credential::ApiKey(ApiKeyCredential::new("sk-test"))
        );
        assert_eq!(serde_json::to_string(&credential).unwrap(), json);
    }

    #[test]
    fn oauth_credential_round_trips_unknown_provider_fields() {
        let json = r#"{"type":"oauth","refresh":"r","access":"a","expires":123,"enterpriseUrl":"company.ghe.com"}"#;
        let credential: Credential = serde_json::from_str(json).unwrap();
        let oauth = credential.as_oauth().expect("oauth credential");
        assert_eq!(oauth.access, "a");
        assert_eq!(oauth.expires, 123);
        assert_eq!(oauth.extra_str("enterpriseUrl"), Some("company.ghe.com"));
        assert_eq!(serde_json::to_string(&credential).unwrap(), json);
    }

    #[test]
    fn api_key_credential_keeps_provider_env() {
        let json = r#"{"type":"api_key","key":"$SCOPED_KEY","env":{"REGION":"test-region"}}"#;
        let credential: Credential = serde_json::from_str(json).unwrap();
        let api_key = credential.as_api_key().expect("api key credential");
        assert_eq!(
            api_key.env.get("REGION").map(String::as_str),
            Some("test-region")
        );
        assert_eq!(serde_json::to_string(&credential).unwrap(), json);
    }
}
