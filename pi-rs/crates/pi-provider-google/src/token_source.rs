//! Google Cloud access tokens for the Vertex adapter.
//!
//! Upstream hands the whole problem to `google-auth-library` through
//! `GoogleGenAI({ vertexai: true, googleAuthOptions })`. There is no equivalent
//! crate in this workspace, and interactive login belongs to `pi-auth`, so the
//! adapter depends on an object-safe trait instead:
//!
//! ```ignore
//! let client = GoogleVertexClient::new().with_token_source(my_source);
//! ```
//!
//! Implemented here (the ambient, non-interactive paths):
//! - [`EnvTokenSource`] — a pre-minted token in `GOOGLE_CLOUD_ACCESS_TOKEN` /
//!   `GOOGLE_OAUTH_ACCESS_TOKEN`, which is also how `gcloud auth print-access-token`
//!   output gets injected.
//! - [`AdcFileTokenSource`] — `GOOGLE_APPLICATION_CREDENTIALS` or the well-known
//!   ADC path, for both `authorized_user` and `service_account` files.
//! - [`MetadataServerTokenSource`] — the GCE/Cloud Run metadata server.
//!
//! Deliberately absent: `gcloud auth application-default login` (interactive),
//! workload identity federation (`external_account`), and impersonated service
//! accounts. Those belong to `pi-auth`; they plug in as another implementation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pi_core::message::now_ms;
use pi_core::options::{AbortSignal, ProviderEnv};
use pi_core::AiError;
use pi_http::client::{HttpClient, JsonRequest};

/// The scope Vertex AI requests are authorized with.
pub const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const DEFAULT_METADATA_HOST: &str = "http://metadata.google.internal";
const WELL_KNOWN_ADC_SUFFIX: &str = ".config/gcloud/application_default_credentials.json";
/// Refresh a little early so a token cannot expire between mint and use.
const EXPIRY_SKEW_MS: i64 = 60_000;

/// What the caller knows about the request needing a token.
#[derive(Debug, Clone, Default)]
pub struct GoogleTokenRequest {
    pub scopes: Vec<String>,
    /// Provider-scoped environment overrides; these win over the process env.
    pub env: ProviderEnv,
    pub signal: Option<AbortSignal>,
}

impl GoogleTokenRequest {
    pub fn cloud_platform(env: ProviderEnv, signal: Option<AbortSignal>) -> Self {
        Self {
            scopes: vec![CLOUD_PLATFORM_SCOPE.to_string()],
            env,
            signal,
        }
    }

    pub fn scope_string(&self) -> String {
        if self.scopes.is_empty() {
            CLOUD_PLATFORM_SCOPE.to_string()
        } else {
            self.scopes.join(" ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleAccessToken {
    pub token: String,
    /// Unix ms. `None` means "unknown", which disables caching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub token_type: String,
}

impl GoogleAccessToken {
    pub fn bearer(token: impl Into<String>, expires_at_ms: Option<i64>) -> Self {
        Self {
            token: token.into(),
            expires_at_ms,
            token_type: "Bearer".to_string(),
        }
    }

    /// The `Authorization` header value.
    pub fn header_value(&self) -> String {
        format!("{} {}", self.token_type, self.token)
    }

    fn is_fresh(&self) -> bool {
        match self.expires_at_ms {
            Some(expiry) => expiry - EXPIRY_SKEW_MS > now_ms(),
            None => false,
        }
    }
}

/// How the Vertex adapter obtains OAuth2 access tokens.
///
/// Object-safe by construction so it can be stored as
/// `Arc<dyn GoogleTokenSource>` and swapped by `pi-auth` at runtime.
#[async_trait]
pub trait GoogleTokenSource: Send + Sync + 'static {
    /// Stable identifier, used in error text so a failed chain is diagnosable.
    fn id(&self) -> &str;

    async fn token(&self, request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError>;
}

/// Shared handle, mirroring the other extension points in the SDK.
pub type GoogleTokenSourceRef = Arc<dyn GoogleTokenSource>;

fn env_value(name: &str, env: &ProviderEnv) -> Option<String> {
    env.get(name)
        .cloned()
        .or_else(|| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// --- static -----------------------------------------------------------------

/// A fixed token. Useful for tests and for callers that mint tokens elsewhere.
#[derive(Debug, Clone)]
pub struct StaticTokenSource {
    token: GoogleAccessToken,
}

impl StaticTokenSource {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: GoogleAccessToken::bearer(token, None),
        }
    }
}

#[async_trait]
impl GoogleTokenSource for StaticTokenSource {
    fn id(&self) -> &str {
        "static"
    }

    async fn token(&self, _request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError> {
        Ok(self.token.clone())
    }
}

// --- environment ------------------------------------------------------------

/// Reads an already-minted access token out of the environment.
#[derive(Debug, Clone, Default)]
pub struct EnvTokenSource {
    names: Vec<String>,
}

impl EnvTokenSource {
    pub fn new() -> Self {
        Self {
            names: vec![
                "GOOGLE_CLOUD_ACCESS_TOKEN".to_string(),
                "GOOGLE_OAUTH_ACCESS_TOKEN".to_string(),
            ],
        }
    }

    pub fn with_names(names: Vec<String>) -> Self {
        Self { names }
    }
}

#[async_trait]
impl GoogleTokenSource for EnvTokenSource {
    fn id(&self) -> &str {
        "env"
    }

    async fn token(&self, request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError> {
        for name in &self.names {
            if let Some(token) = env_value(name, &request.env) {
                return Ok(GoogleAccessToken::bearer(token, None));
            }
        }
        Err(AiError::auth(format!(
            "no access token in {}",
            self.names.join(" / ")
        )))
    }
}

// --- application default credentials file -----------------------------------

#[derive(Debug, Clone, Deserialize)]
struct AdcFile {
    #[serde(rename = "type")]
    kind: Option<String>,
    // authorized_user
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    // service_account
    client_email: Option<String>,
    private_key: Option<String>,
    token_uri: Option<String>,
}

/// Reads `GOOGLE_APPLICATION_CREDENTIALS` (or the well-known ADC path) and
/// exchanges it for an access token.
#[derive(Clone)]
pub struct AdcFileTokenSource {
    http: Arc<HttpClient>,
    /// Explicit path, overriding both the env var and the well-known location.
    path: Option<PathBuf>,
    /// Overrides the credential file's `token_uri`. Tests point this at wiremock.
    token_uri_override: Option<String>,
}

impl AdcFileTokenSource {
    pub fn new(http: Arc<HttpClient>) -> Self {
        Self {
            http,
            path: None,
            token_uri_override: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_token_uri(mut self, uri: impl Into<String>) -> Self {
        self.token_uri_override = Some(uri.into());
        self
    }

    fn resolve_path(&self, env: &ProviderEnv) -> Option<PathBuf> {
        if let Some(path) = &self.path {
            return Some(path.clone());
        }
        if let Some(path) = env_value("GOOGLE_APPLICATION_CREDENTIALS", env) {
            return Some(PathBuf::from(expand_home(&path, env)));
        }
        let home = env_value("HOME", env).or_else(|| env_value("USERPROFILE", env))?;
        Some(PathBuf::from(home).join(WELL_KNOWN_ADC_SUFFIX))
    }
}

fn expand_home(path: &str, env: &ProviderEnv) -> String {
    let Some(rest) = path.strip_prefix("~/") else {
        return path.to_string();
    };
    match env_value("HOME", env).or_else(|| env_value("USERPROFILE", env)) {
        Some(home) => format!("{home}/{rest}"),
        None => path.to_string(),
    }
}

#[async_trait]
impl GoogleTokenSource for AdcFileTokenSource {
    fn id(&self) -> &str {
        "adc-file"
    }

    async fn token(&self, request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError> {
        let path = self.resolve_path(&request.env).ok_or_else(|| {
            AiError::auth(
                "no application default credentials path (set GOOGLE_APPLICATION_CREDENTIALS)",
            )
        })?;
        let raw = tokio::fs::read_to_string(&path).await.map_err(|err| {
            AiError::auth(format!(
                "cannot read Google credentials at {}: {err}",
                path.display()
            ))
        })?;
        let file: AdcFile = serde_json::from_str(&raw).map_err(|err| {
            AiError::auth(format!(
                "invalid Google credentials at {}: {err}",
                path.display()
            ))
        })?;

        match file.kind.as_deref() {
            Some("authorized_user") => self.refresh_authorized_user(&file, request).await,
            Some("service_account") => self.exchange_service_account(&file, request).await,
            Some(other) => Err(AiError::unsupported(format!(
                "Google credential type \"{other}\" is not supported by pi-provider-google; \
                 supply a GoogleTokenSource that handles it"
            ))),
            None => Err(AiError::auth(format!(
                "Google credentials at {} have no \"type\"",
                path.display()
            ))),
        }
    }
}

impl AdcFileTokenSource {
    fn token_uri(&self, file: &AdcFile) -> String {
        self.token_uri_override
            .clone()
            .or_else(|| file.token_uri.clone())
            .unwrap_or_else(|| DEFAULT_TOKEN_URI.to_string())
    }

    async fn refresh_authorized_user(
        &self,
        file: &AdcFile,
        request: &GoogleTokenRequest,
    ) -> Result<GoogleAccessToken, AiError> {
        let (Some(client_id), Some(client_secret), Some(refresh_token)) = (
            file.client_id.as_ref(),
            file.client_secret.as_ref(),
            file.refresh_token.as_ref(),
        ) else {
            return Err(AiError::auth(
                "authorized_user credentials need client_id, client_secret and refresh_token",
            ));
        };
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": client_id,
            "client_secret": client_secret,
            "refresh_token": refresh_token,
        });
        self.post_token(self.token_uri(file), body, request).await
    }

    async fn exchange_service_account(
        &self,
        file: &AdcFile,
        request: &GoogleTokenRequest,
    ) -> Result<GoogleAccessToken, AiError> {
        let (Some(client_email), Some(private_key)) =
            (file.client_email.as_ref(), file.private_key.as_ref())
        else {
            return Err(AiError::auth(
                "service_account credentials need client_email and private_key",
            ));
        };
        let token_uri = self.token_uri(file);
        // The audience is always the credential file's own token_uri, even when
        // the transport is redirected for tests.
        let audience = file
            .token_uri
            .clone()
            .unwrap_or_else(|| DEFAULT_TOKEN_URI.to_string());
        let assertion = sign_service_account_jwt(
            client_email,
            private_key,
            &audience,
            &request.scope_string(),
        )?;
        let body = serde_json::json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
            "assertion": assertion,
        });
        self.post_token(token_uri, body, request).await
    }

    async fn post_token(
        &self,
        url: String,
        body: Value,
        request: &GoogleTokenRequest,
    ) -> Result<GoogleAccessToken, AiError> {
        let req = JsonRequest::post(url, body).signal(request.signal.clone());
        let response = self
            .http
            .post_json(req)
            .await
            .map_err(|err| AiError::auth(format!("Google token exchange failed: {err}")))?;
        parse_token_response(&response.body)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
}

fn parse_token_response(body: &str) -> Result<GoogleAccessToken, AiError> {
    let parsed: TokenResponse = serde_json::from_str(body)
        .map_err(|err| AiError::auth(format!("malformed Google token response: {err}")))?;
    Ok(GoogleAccessToken {
        expires_at_ms: parsed.expires_in.map(|secs| now_ms() + secs * 1000),
        token_type: parsed.token_type.unwrap_or_else(|| "Bearer".to_string()),
        token: parsed.access_token,
    })
}

/// RS256-sign the `urn:ietf:params:oauth:grant-type:jwt-bearer` assertion.
fn sign_service_account_jwt(
    client_email: &str,
    private_key_pem: &str,
    audience: &str,
    scope: &str,
) -> Result<String, AiError> {
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use rsa::RsaPrivateKey;
    use sha2::Sha256;

    // Service account JSON escapes newlines; unescape before PEM parsing.
    let pem = private_key_pem.replace("\\n", "\n");
    let key = RsaPrivateKey::from_pkcs8_pem(pem.trim()).map_err(|err| {
        AiError::auth(format!(
            "service account private_key is not a PKCS#8 RSA key: {err}"
        ))
    })?;

    let issued_at = now_ms() / 1000;
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": client_email,
        "scope": scope,
        "aud": audience,
        "iat": issued_at,
        "exp": issued_at + 3600,
    });

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let signing_input = format!(
        "{}.{}",
        engine.encode(serde_json::to_vec(&header).unwrap_or_default()),
        engine.encode(serde_json::to_vec(&claims).unwrap_or_default())
    );
    let signature = SigningKey::<Sha256>::new(key).sign(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        engine.encode(signature.to_bytes())
    ))
}

// --- metadata server --------------------------------------------------------

/// The GCE / Cloud Run / GKE metadata server.
#[derive(Clone)]
pub struct MetadataServerTokenSource {
    http: Arc<HttpClient>,
    base_url: Option<String>,
}

impl MetadataServerTokenSource {
    pub fn new(http: Arc<HttpClient>) -> Self {
        Self {
            http,
            base_url: None,
        }
    }

    /// Overrides the host. Also honours `GCE_METADATA_HOST` at call time.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    fn base(&self, env: &ProviderEnv) -> String {
        if let Some(base) = &self.base_url {
            return base.trim_end_matches('/').to_string();
        }
        match env_value("GCE_METADATA_HOST", env) {
            // The documented variable is a bare host, not a URL.
            Some(host) if host.starts_with("http") => host.trim_end_matches('/').to_string(),
            Some(host) => format!("http://{}", host.trim_end_matches('/')),
            None => DEFAULT_METADATA_HOST.to_string(),
        }
    }
}

#[async_trait]
impl GoogleTokenSource for MetadataServerTokenSource {
    fn id(&self) -> &str {
        "metadata-server"
    }

    async fn token(&self, request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError> {
        let url = format!(
            "{}/computeMetadata/v1/instance/service-accounts/default/token",
            self.base(&request.env)
        );
        let req = JsonRequest::get(url)
            .header("Metadata-Flavor", "Google")
            .signal(request.signal.clone());
        let response = self.http.get_json(req).await.map_err(|err| {
            AiError::auth(format!(
                "Google metadata server token request failed: {err}"
            ))
        })?;
        parse_token_response(&response.body)
    }
}

// --- composition ------------------------------------------------------------

/// Tries each source in order, reporting every failure if none succeed.
pub struct ChainTokenSource {
    sources: Vec<GoogleTokenSourceRef>,
}

impl ChainTokenSource {
    pub fn new(sources: Vec<GoogleTokenSourceRef>) -> Self {
        Self { sources }
    }
}

#[async_trait]
impl GoogleTokenSource for ChainTokenSource {
    fn id(&self) -> &str {
        "chain"
    }

    async fn token(&self, request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError> {
        let mut failures = Vec::new();
        for source in &self.sources {
            match source.token(request).await {
                Ok(token) => return Ok(token),
                Err(err) => failures.push(format!("{}: {}", source.id(), err.message())),
            }
        }
        Err(AiError::auth(format!(
            "no Google Cloud credentials available ({})",
            failures.join("; ")
        )))
    }
}

/// Caches the wrapped source's token until shortly before it expires.
///
/// Tokens without a known expiry are never cached, so a static or env token
/// keeps reflecting its source.
pub struct CachingTokenSource {
    inner: GoogleTokenSourceRef,
    cached: tokio::sync::Mutex<Option<GoogleAccessToken>>,
}

impl CachingTokenSource {
    pub fn new(inner: GoogleTokenSourceRef) -> Self {
        Self {
            inner,
            cached: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl GoogleTokenSource for CachingTokenSource {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn token(&self, request: &GoogleTokenRequest) -> Result<GoogleAccessToken, AiError> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref() {
            if token.is_fresh() {
                return Ok(token.clone());
            }
        }
        let token = self.inner.token(request).await?;
        if token.is_fresh() {
            *cached = Some(token.clone());
        } else {
            *cached = None;
        }
        Ok(token)
    }
}

/// The ambient chain: environment token, then ADC file, then metadata server.
pub fn default_token_source(http: Arc<HttpClient>) -> GoogleTokenSourceRef {
    Arc::new(CachingTokenSource::new(Arc::new(ChainTokenSource::new(
        vec![
            Arc::new(EnvTokenSource::new()),
            Arc::new(AdcFileTokenSource::new(http.clone())),
            Arc::new(MetadataServerTokenSource::new(http)),
        ],
    ))))
}

/// Env-var names the Vertex adapter and `pi-auth` both care about.
pub fn vertex_env_names() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("project", "GOOGLE_CLOUD_PROJECT"),
        ("projectAlt", "GCLOUD_PROJECT"),
        ("location", "GOOGLE_CLOUD_LOCATION"),
        ("apiKey", "GOOGLE_CLOUD_API_KEY"),
        ("credentials", "GOOGLE_APPLICATION_CREDENTIALS"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn env_source_reads_provider_env_first() {
        let mut env = ProviderEnv::new();
        env.insert("GOOGLE_CLOUD_ACCESS_TOKEN".into(), "scoped".into());
        let source = EnvTokenSource::new();
        let token = source
            .token(&GoogleTokenRequest::cloud_platform(env, None))
            .await
            .unwrap();
        assert_eq!(token.token, "scoped");
        assert_eq!(token.header_value(), "Bearer scoped");
    }

    #[tokio::test]
    async fn env_source_errors_when_unset() {
        let source = EnvTokenSource::with_names(vec!["PI_TEST_TOKEN_DEFINITELY_UNSET".into()]);
        let err = source
            .token(&GoogleTokenRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "auth");
    }

    #[tokio::test]
    async fn chain_reports_every_failure() {
        let chain = ChainTokenSource::new(vec![
            Arc::new(EnvTokenSource::with_names(vec!["PI_TEST_A".into()])),
            Arc::new(EnvTokenSource::with_names(vec!["PI_TEST_B".into()])),
        ]);
        let err = chain
            .token(&GoogleTokenRequest::default())
            .await
            .unwrap_err();
        assert!(err.message().contains("PI_TEST_A"));
        assert!(err.message().contains("PI_TEST_B"));
    }

    #[test]
    fn token_response_parses_expiry() {
        let token = parse_token_response(r#"{"access_token":"t","expires_in":3600}"#).unwrap();
        assert_eq!(token.token, "t");
        assert!(token.expires_at_ms.unwrap() > now_ms());
        assert!(token.is_fresh());
    }

    #[test]
    fn tilde_paths_expand_against_home() {
        let mut env = ProviderEnv::new();
        env.insert("HOME".into(), "/home/pi".into());
        assert_eq!(expand_home("~/creds.json", &env), "/home/pi/creds.json");
        assert_eq!(expand_home("/abs/creds.json", &env), "/abs/creds.json");
    }
}
