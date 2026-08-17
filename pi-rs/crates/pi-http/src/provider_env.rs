//! Provider-scoped environment lookup. Port of `packages/ai/src/utils/provider-env.ts`.
//!
//! Resolution order is scoped override, then the process environment. Empty
//! strings count as absent, because upstream chains the lookups with `||`.
//!
//! Upstream's third fallback — reading `/proc/self/environ` to work around a Bun
//! sandbox bug — has no analogue here and is not ported.

use pi_core::options::ProviderEnv;

/// Resolve `name` from the scoped overrides first, then the process env.
pub fn get_provider_env_value(name: &str, env: &ProviderEnv) -> Option<String> {
    non_empty(env.get(name).cloned()).or_else(|| non_empty(std::env::var(name).ok()))
}

/// [`get_provider_env_value`] with the process environment excluded. Useful when
/// a caller must not be influenced by ambient configuration.
pub fn get_scoped_env_value(name: &str, env: &ProviderEnv) -> Option<String> {
    non_empty(env.get(name).cloned())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_overrides_win_over_the_process_env() {
        // Chosen to be absent from any real environment.
        let key = "PI_RS_TEST_PROVIDER_ENV_A";
        std::env::set_var(key, "from-process");
        let env: ProviderEnv = [(key.to_string(), "from-scope".to_string())]
            .into_iter()
            .collect();
        assert_eq!(
            get_provider_env_value(key, &env).as_deref(),
            Some("from-scope")
        );
        std::env::remove_var(key);
    }

    #[test]
    fn an_empty_scoped_value_falls_through_to_the_process_env() {
        let key = "PI_RS_TEST_PROVIDER_ENV_B";
        std::env::set_var(key, "from-process");
        let env: ProviderEnv = [(key.to_string(), String::new())].into_iter().collect();
        assert_eq!(
            get_provider_env_value(key, &env).as_deref(),
            Some("from-process")
        );
        std::env::remove_var(key);
    }

    #[test]
    fn missing_everywhere_is_none() {
        assert_eq!(
            get_provider_env_value("PI_RS_TEST_PROVIDER_ENV_MISSING", &ProviderEnv::new()),
            None
        );
    }
}
