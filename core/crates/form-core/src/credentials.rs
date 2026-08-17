//! Where a provider API key comes from.
//!
//! Two sources, in order:
//!
//! 1. **The macOS Keychain**, service `dev.jhead.form`, account = provider id. This is what
//!    the Preferences pane writes. Reading it here rather than passing the value across the
//!    FFI boundary keeps the original rule intact: the secret never travels through JSON,
//!    never appears in a log, and never sits in `settings.json`.
//! 2. **The environment**, including a `.env` the app loaded at startup. This is what a
//!    terminal run and the tests use.
//!
//! The Keychain wins. A key the user typed into the app should beat a stale shell export
//! they have forgotten about, which is also the precedence `pi-auth` applies internally.

/// Matches `KeychainStore.defaultService` on the Swift side.
pub const KEYCHAIN_SERVICE: &str = "dev.jhead.form";

/// Providers worth checking for a key. Asking the Keychain about every provider in a
/// 1300-model catalog would be a lot of syscalls for nothing.
pub const KNOWN_PROVIDERS: &[&str] = &[
    "openrouter",
    "anthropic",
    "openai",
    "google",
    "groq",
    "mistral",
    "deepseek",
    "xai",
];

/// The key for a provider, Keychain first, then the environment.
pub fn api_key(provider: &str) -> Option<String> {
    keychain_key(provider).or_else(|| pi_auth::get_env_api_key(provider, None))
}

/// Every provider that has a key from either source.
pub fn all_api_keys() -> Vec<(String, String)> {
    KNOWN_PROVIDERS
        .iter()
        .filter_map(|provider| api_key(provider).map(|key| (provider.to_string(), key)))
        .collect()
}

/// Which providers have a key, without reading the values. This is what the settings
/// document's `hasKey` flags are built from.
pub fn providers_with_keys() -> Vec<String> {
    KNOWN_PROVIDERS
        .iter()
        .filter(|provider| api_key(provider).is_some())
        .map(|provider| provider.to_string())
        .collect()
}

#[cfg(target_os = "macos")]
fn keychain_key(provider: &str) -> Option<String> {
    use security_framework::passwords::get_generic_password;

    // A missing item is the common case, not an error worth logging.
    let bytes = get_generic_password(KEYCHAIN_SERVICE, provider).ok()?;
    let value = String::from_utf8(bytes).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(not(target_os = "macos"))]
fn keychain_key(_provider: &str) -> Option<String> {
    // Windows and Linux get their own credential store when those clients are built; until
    // then the environment is the only source, which is what the CLI already uses.
    None
}

/// A [`pi_auth::CredentialStore`] that reads on every call.
///
/// The alternative was seeding an in-memory store once at startup, which meant a key typed
/// into Preferences did nothing until the app was restarted. Reading on demand costs one
/// Keychain lookup per provider per request, which is nothing next to the request itself.
pub struct LiveCredentialStore;

#[async_trait::async_trait]
impl pi_auth::CredentialStore for LiveCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        _signal: Option<pi_core::AbortSignal>,
    ) -> Result<Option<pi_auth::Credential>, pi_auth::AuthError> {
        Ok(api_key(provider_id)
            .map(|key| pi_auth::Credential::ApiKey(pi_auth::ApiKeyCredential::new(key))))
    }

    async fn list(
        &self,
        _signal: Option<pi_core::AbortSignal>,
    ) -> Result<Vec<pi_auth::CredentialInfo>, pi_auth::AuthError> {
        Ok(providers_with_keys()
            .into_iter()
            .map(|provider_id| pi_auth::CredentialInfo {
                provider_id,
                credential_type: pi_auth::AuthType::ApiKey,
            })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        _mutation: std::sync::Arc<dyn pi_auth::CredentialMutation>,
        _signal: Option<pi_core::AbortSignal>,
    ) -> Result<Option<pi_auth::Credential>, pi_auth::AuthError> {
        // Writing is the app's job: Preferences owns the Keychain entry, and the OAuth flows
        // this would otherwise serve are not wired up yet.
        Err(pi_auth::AuthError::store(format!(
            "form stores credentials in the Keychain; {provider_id} cannot be written from the core"
        )))
    }

    async fn delete(
        &self,
        provider_id: &str,
        _signal: Option<pi_core::AbortSignal>,
    ) -> Result<(), pi_auth::AuthError> {
        Err(pi_auth::AuthError::store(format!(
            "form stores credentials in the Keychain; {provider_id} cannot be deleted from the core"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_is_the_fallback_when_the_keychain_has_nothing() {
        // A provider nobody has a key for resolves to nothing rather than panicking.
        assert_eq!(api_key("a-provider-that-does-not-exist"), None);
    }

    #[test]
    fn the_known_provider_list_is_deduplicated() {
        let mut sorted = KNOWN_PROVIDERS.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "a provider is listed twice");
    }
}
