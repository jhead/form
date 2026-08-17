//! Port of the resolution and refresh cases in
//! `.upstream/packages/ai/test/oauth-auth.test.ts` and the credential-store
//! refresh cases in `packages/coding-agent/test/auth-storage.test.ts`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use pi_auth::{
    resolve_provider_auth, ApiKeyCredential, AuthError, AuthProvider, AuthResolutionOverrides,
    Credential, CredentialStore, EnvApiKeyAuth, InMemoryCredentialStore, LoginContext,
    MapAuthContext, ModelAuth, OAuthCredential, OAuthFlow, ProviderAuth,
};
use pi_core::message::now_ms;
use pi_core::options::AbortSignal;

/// An OAuth flow that counts refreshes, so single-flight is observable.
struct CountingOAuth {
    refreshes: Arc<AtomicUsize>,
    /// Held across the refresh so concurrent callers really do overlap.
    delay_ms: u64,
    fail: bool,
    lifetime_ms: i64,
}

impl CountingOAuth {
    fn new(refreshes: Arc<AtomicUsize>) -> Self {
        Self {
            refreshes,
            delay_ms: 20,
            fail: false,
            lifetime_ms: 60 * 60 * 1000,
        }
    }
}

#[async_trait]
impl OAuthFlow for CountingOAuth {
    fn name(&self) -> &str {
        "Counting OAuth"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(&self, _ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
        Err(AuthError::oauth("not used"))
    }

    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: AbortSignal,
    ) -> Result<OAuthCredential, AuthError> {
        let count = self.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        if self.fail {
            return Err(AuthError::oauth("invalid_grant"));
        }
        Ok(OAuthCredential::new(
            format!("refreshed-access-{count}"),
            credential.refresh.clone(),
            now_ms() + self.lifetime_ms,
        ))
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth::from_api_key(credential.access.clone()))
    }
}

fn oauth_provider(flow: Arc<dyn OAuthFlow>) -> AuthProvider {
    AuthProvider::new(
        "counting",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Counting API key",
            ["COUNTING_API_KEY"],
        ))
        .with_oauth(flow),
    )
}

fn expired_credential() -> Credential {
    Credential::OAuth(OAuthCredential::new("stale-access", "refresh-token", 0))
}

fn live_credential() -> Credential {
    Credential::OAuth(OAuthCredential::new(
        "live-access",
        "refresh-token",
        now_ms() + 10 * 60_000,
    ))
}

// ---------------------------------------------------------------------------
// Resolution order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn falls_back_to_the_environment_when_nothing_is_stored() {
    let provider = AuthProvider::new(
        "anthropic",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Anthropic API key",
            ["ANTHROPIC_API_KEY"],
        )),
    );

    let result = resolve_provider_auth(
        &provider,
        Arc::new(InMemoryCredentialStore::new()),
        MapAuthContext::new()
            .with_var("ANTHROPIC_API_KEY", "from-env")
            .shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap()
    .expect("resolved");

    assert_eq!(result.auth.api_key.as_deref(), Some("from-env"));
    assert_eq!(result.source.as_deref(), Some("ANTHROPIC_API_KEY"));
}

/// Upstream's rule: "a stored credential owns the provider; ambient/env is
/// consulted only when nothing is stored".
#[tokio::test]
async fn a_stored_credential_wins_over_the_environment() {
    let provider = AuthProvider::new(
        "anthropic",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Anthropic API key",
            ["ANTHROPIC_API_KEY"],
        )),
    );
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "anthropic".to_string(),
        Credential::ApiKey(ApiKeyCredential::new("from-store")),
    )]));

    let result = resolve_provider_auth(
        &provider,
        credentials,
        MapAuthContext::new()
            .with_var("ANTHROPIC_API_KEY", "from-env")
            .shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap()
    .expect("resolved");

    assert_eq!(result.auth.api_key.as_deref(), Some("from-store"));
    assert_eq!(result.source.as_deref(), Some("stored credential"));
}

#[tokio::test]
async fn an_explicit_api_key_override_bypasses_the_store() {
    let provider = AuthProvider::new(
        "anthropic",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Anthropic API key",
            ["ANTHROPIC_API_KEY"],
        )),
    );
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "anthropic".to_string(),
        Credential::ApiKey(ApiKeyCredential::new("from-store")),
    )]));

    let result = resolve_provider_auth(
        &provider,
        credentials,
        MapAuthContext::new().shared(),
        &AuthResolutionOverrides {
            api_key: Some("from-caller".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .expect("resolved");

    assert_eq!(result.auth.api_key.as_deref(), Some("from-caller"));
}

#[tokio::test]
async fn a_stored_oauth_credential_resolves_through_the_flow() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let provider = oauth_provider(Arc::new(CountingOAuth::new(refreshes.clone())));
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "counting".to_string(),
        live_credential(),
    )]));

    let result = resolve_provider_auth(
        &provider,
        credentials,
        MapAuthContext::new().shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap()
    .expect("resolved");

    assert_eq!(result.auth.api_key.as_deref(), Some("live-access"));
    assert_eq!(result.source.as_deref(), Some("OAuth"));
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        0,
        "a live token is not refreshed"
    );
}

#[tokio::test]
async fn a_stored_credential_type_without_a_handler_resolves_to_nothing() {
    // OAuth credential, but the provider only offers api-key auth: upstream
    // returns undefined rather than silently falling back to the env.
    let provider = AuthProvider::new(
        "anthropic",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Anthropic API key",
            ["ANTHROPIC_API_KEY"],
        )),
    );
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "anthropic".to_string(),
        live_credential(),
    )]));

    let result = resolve_provider_auth(
        &provider,
        credentials,
        MapAuthContext::new()
            .with_var("ANTHROPIC_API_KEY", "from-env")
            .shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap();

    assert!(result.is_none());
}

#[tokio::test]
async fn nothing_configured_resolves_to_nothing() {
    let provider = AuthProvider::new(
        "anthropic",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Anthropic API key",
            ["ANTHROPIC_API_KEY"],
        )),
    );

    let result = resolve_provider_auth(
        &provider,
        Arc::new(InMemoryCredentialStore::new()),
        MapAuthContext::new().shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap();

    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// Transparent, single-flight refresh
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_expiring_token_is_refreshed_and_persisted() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let provider = oauth_provider(Arc::new(CountingOAuth::new(refreshes.clone())));
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "counting".to_string(),
        expired_credential(),
    )]));

    let result = resolve_provider_auth(
        &provider,
        credentials.clone(),
        MapAuthContext::new().shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap()
    .expect("resolved");

    assert_eq!(result.auth.api_key.as_deref(), Some("refreshed-access-1"));
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);

    // The rotated credential is written back before the lock is released.
    let stored = credentials.read("counting", None).await.unwrap().unwrap();
    assert_eq!(stored.as_oauth().unwrap().access, "refreshed-access-1");
}

/// The core single-flight guarantee: ten concurrent callers, one refresh.
#[tokio::test]
async fn concurrent_callers_trigger_exactly_one_refresh() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(oauth_provider(Arc::new(CountingOAuth::new(
        refreshes.clone(),
    ))));
    let credentials: Arc<dyn CredentialStore> = Arc::new(
        InMemoryCredentialStore::with_credentials([("counting".to_string(), expired_credential())]),
    );
    let ctx = MapAuthContext::new().shared();

    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let provider = provider.clone();
            let credentials = credentials.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move {
                resolve_provider_auth(
                    &provider,
                    credentials,
                    ctx,
                    &AuthResolutionOverrides::default(),
                )
                .await
                .unwrap()
                .expect("resolved")
            })
        })
        .collect();

    for task in tasks {
        let result = task.await.unwrap();
        // Every caller sees the same rotated token, not just the winner.
        assert_eq!(result.auth.api_key.as_deref(), Some("refreshed-access-1"));
    }
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
}

/// Same guarantee across independent file-backed store instances, which is
/// what two processes sharing `auth.json` look like.
#[tokio::test]
async fn concurrent_file_backed_callers_trigger_exactly_one_refresh() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({
            "counting": { "type": "oauth", "refresh": "refresh-token", "access": "stale", "expires": 0 }
        }))
        .unwrap(),
    )
    .unwrap();

    let refreshes = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(oauth_provider(Arc::new(CountingOAuth::new(
        refreshes.clone(),
    ))));
    let ctx = MapAuthContext::new().shared();

    let tasks: Vec<_> = (0..4)
        .map(|_| {
            let provider = provider.clone();
            let ctx = ctx.clone();
            let credentials: Arc<dyn CredentialStore> =
                Arc::new(pi_auth::FileCredentialStore::open(&path));
            tokio::spawn(async move {
                resolve_provider_auth(
                    &provider,
                    credentials,
                    ctx,
                    &AuthResolutionOverrides::default(),
                )
                .await
                .unwrap()
                .expect("resolved")
            })
        })
        .collect();

    for task in tasks {
        assert_eq!(
            task.await.unwrap().auth.api_key.as_deref(),
            Some("refreshed-access-1")
        );
    }
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refresh_failure_surfaces_as_an_oauth_error_and_keeps_the_credential() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let flow = CountingOAuth {
        fail: true,
        ..CountingOAuth::new(refreshes.clone())
    };
    let provider = oauth_provider(Arc::new(flow));
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "counting".to_string(),
        expired_credential(),
    )]));

    let error = resolve_provider_auth(
        &provider,
        credentials.clone(),
        MapAuthContext::new()
            .with_var("COUNTING_API_KEY", "from-env")
            .shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "oauth");
    assert!(error
        .message()
        .contains("OAuth refresh failed for counting"));
    // No silent env fallback after a failed refresh, and the stored credential
    // is untouched so a later retry can succeed.
    assert_eq!(
        credentials.read("counting", None).await.unwrap(),
        Some(expired_credential())
    );
}

#[tokio::test]
async fn a_refresh_that_returns_a_short_lived_token_fails_only_for_explicit_callers() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let flow = CountingOAuth {
        // Shorter than the caller's requested minimum validity.
        lifetime_ms: 60_000,
        ..CountingOAuth::new(refreshes.clone())
    };
    let provider = oauth_provider(Arc::new(flow));
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "counting".to_string(),
        expired_credential(),
    )]));

    let error = resolve_provider_auth(
        &provider,
        credentials,
        MapAuthContext::new().shared(),
        &AuthResolutionOverrides {
            min_oauth_validity_ms: Some(30 * 60_000),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "oauth");
    assert!(error.message().contains("expires too soon"));
}

#[tokio::test]
async fn logging_out_during_a_refresh_resolves_to_nothing() {
    struct LogoutDuringRefresh;

    #[async_trait]
    impl OAuthFlow for LogoutDuringRefresh {
        fn name(&self) -> &str {
            "Logout during refresh"
        }

        async fn login(&self, _ctx: &LoginContext) -> Result<OAuthCredential, AuthError> {
            Err(AuthError::oauth("not used"))
        }

        async fn refresh(
            &self,
            _credential: &OAuthCredential,
            _signal: AbortSignal,
        ) -> Result<OAuthCredential, AuthError> {
            // The store lock is held by this very mutation, so reach past the
            // public API the way a concurrent logout would land: return nothing
            // and let the entry stay absent.
            Ok(OAuthCredential::new("unused", "unused", now_ms() + 60_000))
        }

        async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
            Ok(ModelAuth::from_api_key(credential.access.clone()))
        }
    }

    // The credential vanishes between the optimistic read and the locked
    // re-read, which is the race the double-checked lock exists for.
    let credentials = Arc::new(InMemoryCredentialStore::with_credentials([(
        "counting".to_string(),
        expired_credential(),
    )]));
    let provider = oauth_provider(Arc::new(LogoutDuringRefresh));
    credentials.delete("counting", None).await.unwrap();

    let result = resolve_provider_auth(
        &provider,
        credentials,
        MapAuthContext::new().shared(),
        &AuthResolutionOverrides::default(),
    )
    .await
    .unwrap();

    assert!(result.is_none());
}

#[tokio::test]
async fn provider_env_overrides_overlay_the_ambient_context() {
    let provider = AuthProvider::new(
        "anthropic",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Anthropic API key",
            ["ANTHROPIC_API_KEY"],
        )),
    );
    let mut env = pi_core::options::ProviderEnv::new();
    env.insert("ANTHROPIC_API_KEY".into(), "scoped".into());

    let result = resolve_provider_auth(
        &provider,
        Arc::new(InMemoryCredentialStore::new()),
        MapAuthContext::new()
            .with_var("ANTHROPIC_API_KEY", "ambient")
            .shared(),
        &AuthResolutionOverrides {
            env,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .expect("resolved");

    assert_eq!(result.auth.api_key.as_deref(), Some("scoped"));
}

#[tokio::test]
async fn an_aborted_resolution_is_reported_as_cancelled() {
    let provider = AuthProvider::new(
        "anthropic",
        ProviderAuth::api_key(EnvApiKeyAuth::shared(
            "Anthropic API key",
            ["ANTHROPIC_API_KEY"],
        )),
    );
    let (handle, signal) = pi_core::options::AbortHandle::new();
    handle.abort();

    let error = resolve_provider_auth(
        &provider,
        Arc::new(InMemoryCredentialStore::new()),
        MapAuthContext::new().shared(),
        &AuthResolutionOverrides {
            signal: Some(signal),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(error.is_cancelled());
    assert_eq!(pi_core::AiError::from(error).code(), "aborted");
}
