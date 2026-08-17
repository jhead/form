//! Port of `packages/ai/src/env-api-keys.ts`.
//!
//! Reports which environment variables can supply a key for a provider, and
//! resolves one. Deliberately excludes ambient credential sources (AWS
//! profiles, IAM, Google ADC) from `find_env_keys` — those only surface through
//! `get_env_api_key`'s `<authenticated>` sentinel.

use pi_core::options::ProviderEnv;

pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Marker returned for providers whose credentials are ambient rather than a
/// key value (Vertex ADC, AWS profiles/IAM).
pub const AUTHENTICATED_SENTINEL: &str = "<authenticated>";

/// `getProviderEnvValue`: provider-scoped overrides, then the process env.
/// Empty strings count as unset, matching the `||` chain upstream.
pub fn provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(value) = env.and_then(|e| e.get(name)) {
        if !value.is_empty() {
            return Some(value.clone());
        }
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The api-key environment variables a provider recognises, in priority order.
pub fn api_key_env_vars(provider: &str) -> Option<Vec<&'static str>> {
    if provider == "github-copilot" {
        return Some(vec!["COPILOT_GITHUB_TOKEN"]);
    }

    // ANTHROPIC_AUTH_TOKEN participates in env discovery/status, but
    // get_env_api_key() skips it because requests must pass it as
    // `Authorization: Bearer`, not as an api key.
    if provider == "anthropic" {
        return Some(vec![
            ANTHROPIC_AUTH_TOKEN_ENV,
            ANTHROPIC_OAUTH_TOKEN_ENV,
            ANTHROPIC_API_KEY_ENV,
        ]);
    }

    let env_var = match provider {
        "ant-ling" => "ANT_LING_API_KEY",
        "qwen-token-plan" => "QWEN_TOKEN_PLAN_API_KEY",
        "qwen-token-plan-cn" => "QWEN_TOKEN_PLAN_CN_API_KEY",
        "qwen-token-plan-individual" => "QWEN_TOKEN_PLAN_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "azure-openai-responses" => "AZURE_OPENAI_API_KEY",
        "nvidia" => "NVIDIA_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "google" => "GEMINI_API_KEY",
        "google-vertex" => "GOOGLE_CLOUD_API_KEY",
        "groq" => "GROQ_API_KEY",
        "cerebras" => "CEREBRAS_API_KEY",
        "xai" => "XAI_API_KEY",
        "radius" => "RADIUS_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "vercel-ai-gateway" => "AI_GATEWAY_API_KEY",
        "zai" => "ZAI_API_KEY",
        "zai-coding-cn" => "ZAI_CODING_CN_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "minimax-cn" => "MINIMAX_CN_API_KEY",
        "moonshotai" => "MOONSHOT_API_KEY",
        "moonshotai-cn" => "MOONSHOT_API_KEY",
        "huggingface" => "HF_TOKEN",
        "fireworks" => "FIREWORKS_API_KEY",
        "together" => "TOGETHER_API_KEY",
        "baseten" => "BASETEN_API_KEY",
        "opencode" => "OPENCODE_API_KEY",
        "opencode-go" => "OPENCODE_API_KEY",
        "kimi-coding" => "KIMI_API_KEY",
        "cloudflare-workers-ai" => "CLOUDFLARE_API_KEY",
        "cloudflare-ai-gateway" => "CLOUDFLARE_API_KEY",
        "xiaomi" => "XIAOMI_API_KEY",
        "xiaomi-token-plan-cn" => "XIAOMI_TOKEN_PLAN_CN_API_KEY",
        "xiaomi-token-plan-ams" => "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
        "xiaomi-token-plan-sgp" => "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        _ => return None,
    };
    Some(vec![env_var])
}

/// Environment variables that are actually set for this provider.
/// `None` when the provider has no known api-key variables at all;
/// `None` too when it has some but none are set (upstream returns `undefined`
/// rather than an empty array).
pub fn find_env_keys(provider: &str, env: Option<&ProviderEnv>) -> Option<Vec<String>> {
    let env_vars = api_key_env_vars(provider)?;
    let found: Vec<String> = env_vars
        .into_iter()
        .filter(|name| provider_env_value(name, env).is_some())
        .map(str::to_string)
        .collect();
    (!found.is_empty()).then_some(found)
}

/// Resolve an api key for the provider from the environment. Never returns a
/// value that must be sent as a bearer token instead of an api key.
pub fn get_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(env_keys) = find_env_keys(provider, env) {
        // ANTHROPIC_AUTH_TOKEN is discovery-only; skip it when picking a key.
        let api_key_env = if provider == "anthropic" {
            env_keys.iter().find(|k| *k != ANTHROPIC_AUTH_TOKEN_ENV)
        } else {
            env_keys.first()
        };
        if let Some(name) = api_key_env {
            return provider_env_value(name, env);
        }
    }

    // Vertex AI accepts either an explicit key or Application Default
    // Credentials configured by `gcloud auth application-default login`.
    if provider == "google-vertex" {
        let has_credentials = has_vertex_adc_credentials(env);
        let has_project = provider_env_value("GOOGLE_CLOUD_PROJECT", env).is_some()
            || provider_env_value("GCLOUD_PROJECT", env).is_some();
        let has_location = provider_env_value("GOOGLE_CLOUD_LOCATION", env).is_some();
        if has_credentials && has_project && has_location {
            return Some(AUTHENTICATED_SENTINEL.to_string());
        }
    }

    if provider == "amazon-bedrock" {
        let configured = provider_env_value("AWS_PROFILE", env).is_some()
            || (provider_env_value("AWS_ACCESS_KEY_ID", env).is_some()
                && provider_env_value("AWS_SECRET_ACCESS_KEY", env).is_some())
            || provider_env_value("AWS_BEARER_TOKEN_BEDROCK", env).is_some()
            || provider_env_value("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", env).is_some()
            || provider_env_value("AWS_CONTAINER_CREDENTIALS_FULL_URI", env).is_some()
            || provider_env_value("AWS_WEB_IDENTITY_TOKEN_FILE", env).is_some();
        if configured {
            return Some(AUTHENTICATED_SENTINEL.to_string());
        }
    }

    None
}

fn has_vertex_adc_credentials(env: Option<&ProviderEnv>) -> bool {
    // Unlike upstream this is not cached: the cache exists there to dodge a
    // dynamic-import race at startup that has no analogue here, and a stale
    // `false` survives for the life of the process.
    if let Some(path) = provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env) {
        return std::path::Path::new(&path).exists();
    }
    let Some(home) = crate::context::home_dir() else {
        return false;
    };
    home.join(".config")
        .join("gcloud")
        .join("application_default_credentials.json")
        .exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ProviderEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // The upstream tests mutate process.env; the port drives the same cases
    // through the provider-scoped overlay, which shares the lookup path and
    // does not make the suite order-dependent.

    #[test]
    fn generic_github_tokens_are_not_copilot_credentials() {
        let scoped = env(&[("GH_TOKEN", "gh-token"), ("GITHUB_TOKEN", "github-token")]);
        assert_eq!(find_env_keys("github-copilot", Some(&scoped)), None);
        assert_eq!(get_env_api_key("github-copilot", Some(&scoped)), None);
    }

    #[test]
    fn copilot_resolves_from_copilot_github_token() {
        let scoped = env(&[
            ("COPILOT_GITHUB_TOKEN", "copilot-token"),
            ("GH_TOKEN", "gh-token"),
        ]);
        assert_eq!(
            find_env_keys("github-copilot", Some(&scoped)),
            Some(vec!["COPILOT_GITHUB_TOKEN".to_string()])
        );
        assert_eq!(
            get_env_api_key("github-copilot", Some(&scoped)).as_deref(),
            Some("copilot-token")
        );
    }

    #[test]
    fn zai_china_coding_plan_resolves_from_its_own_variable() {
        let scoped = env(&[("ZAI_CODING_CN_API_KEY", "zai-coding-cn-token")]);
        assert_eq!(
            find_env_keys("zai-coding-cn", Some(&scoped)),
            Some(vec!["ZAI_CODING_CN_API_KEY".to_string()])
        );
        assert_eq!(
            get_env_api_key("zai-coding-cn", Some(&scoped)).as_deref(),
            Some("zai-coding-cn-token")
        );
    }

    #[test]
    fn anthropic_auth_token_is_reported_but_never_used_as_an_api_key() {
        let all = env(&[
            (ANTHROPIC_AUTH_TOKEN_ENV, "auth-token"),
            (ANTHROPIC_OAUTH_TOKEN_ENV, "oauth-token"),
            (ANTHROPIC_API_KEY_ENV, "api-key"),
        ]);
        assert_eq!(
            find_env_keys("anthropic", Some(&all)),
            Some(vec![
                ANTHROPIC_AUTH_TOKEN_ENV.to_string(),
                ANTHROPIC_OAUTH_TOKEN_ENV.to_string(),
                ANTHROPIC_API_KEY_ENV.to_string(),
            ])
        );
        assert_eq!(
            get_env_api_key("anthropic", Some(&all)).as_deref(),
            Some("oauth-token")
        );

        let only_auth_token = env(&[(ANTHROPIC_AUTH_TOKEN_ENV, "auth-token")]);
        assert_eq!(
            find_env_keys("anthropic", Some(&only_auth_token)),
            Some(vec![ANTHROPIC_AUTH_TOKEN_ENV.to_string()])
        );
        assert_eq!(get_env_api_key("anthropic", Some(&only_auth_token)), None);
    }

    #[test]
    fn anthropic_falls_back_to_the_api_key_variable() {
        let scoped = env(&[(ANTHROPIC_API_KEY_ENV, "api-key")]);
        assert_eq!(
            get_env_api_key("anthropic", Some(&scoped)).as_deref(),
            Some("api-key")
        );
    }

    #[test]
    fn unknown_providers_have_no_env_keys() {
        assert_eq!(api_key_env_vars("not-a-provider"), None);
        assert_eq!(find_env_keys("not-a-provider", None), None);
    }
}
