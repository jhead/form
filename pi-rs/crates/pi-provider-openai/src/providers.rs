//! Provider descriptors for the OpenAI-shaped providers.
//!
//! Port of the `providers/*.ts` entries whose `api` binding is one of this
//! crate's four adapters. Upstream's `Provider` is a live object with auth flows
//! and a model catalog; the catalog lives in `pi-catalog` and the auth flows in
//! `pi-auth`, so what belongs *here* is only the static identity of each
//! provider plus which api it speaks.
//!
//! These are plain data. `pi-catalog` reads them and registers the matching
//! [`pi_core::ApiClient`] from [`crate::all_api_clients`]; nothing in this crate
//! depends on the registry, keeping the dependency edge one-way.

use serde::{Deserialize, Serialize};

/// How a provider is authenticated. Only enough to drive credential lookup —
/// the flows themselves belong to `pi-auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthKind {
    /// Environment variables checked in order for an API key.
    #[serde(default)]
    pub api_key_env: Vec<String>,
    /// Human-readable credential name, e.g. `"OpenAI API key"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_name: Option<String>,
    /// Whether the provider also offers an OAuth flow.
    #[serde(default)]
    pub oauth: bool,
    /// OAuth flow label when there is one, e.g. `"OpenAI (ChatGPT Plus/Pro)"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_name: Option<String>,
    /// Whether the OAuth credential is a consumer subscription rather than a key.
    #[serde(default)]
    pub oauth_is_subscription: bool,
}

impl ProviderAuthKind {
    fn env(name: &str, vars: &[&str]) -> Self {
        Self {
            api_key_env: vars.iter().map(|v| v.to_string()).collect(),
            api_key_name: Some(name.to_string()),
            oauth: false,
            oauth_name: None,
            oauth_is_subscription: false,
        }
    }

    fn with_oauth(mut self, name: &str, is_subscription: bool) -> Self {
        self.oauth = true;
        self.oauth_name = Some(name.to_string());
        self.oauth_is_subscription = is_subscription;
        self
    }

    fn oauth_only(name: &str, is_subscription: bool) -> Self {
        Self {
            api_key_env: Vec::new(),
            api_key_name: None,
            oauth: true,
            oauth_name: Some(name.to_string()),
            oauth_is_subscription: is_subscription,
        }
    }

    fn custom() -> Self {
        Self {
            api_key_env: Vec::new(),
            api_key_name: None,
            oauth: false,
            oauth_name: None,
            oauth_is_subscription: false,
        }
    }
}

/// Static identity of one provider served by this crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: String,
    /// Absent for providers whose endpoint is per-account (Azure, Cloudflare,
    /// OpenCode) and comes from the model or from configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Every api this provider can speak. The first entry is its default.
    pub apis: Vec<String>,
    pub auth: ProviderAuthKind,
}

impl ProviderDescriptor {
    fn new(
        id: &str,
        name: &str,
        base_url: Option<&str>,
        apis: &[&str],
        auth: ProviderAuthKind,
    ) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            base_url: base_url.map(str::to_string),
            apis: apis.iter().map(|a| a.to_string()).collect(),
            auth,
        }
    }

    /// Whether this provider's default api is served by this crate.
    pub fn is_openai_shaped(&self) -> bool {
        self.apis.iter().any(|api| {
            matches!(
                api.as_str(),
                crate::openai_completions::API
                    | crate::openai_responses::API
                    | crate::azure_openai_responses::API
                    | crate::openai_codex_responses::API
            )
        })
    }
}

const COMPLETIONS: &str = crate::openai_completions::API;
const RESPONSES: &str = crate::openai_responses::API;
const AZURE: &str = crate::azure_openai_responses::API;
const CODEX: &str = crate::openai_codex_responses::API;
const ANTHROPIC: &str = "anthropic-messages";
const GOOGLE: &str = "google-generative-ai";

/// Every provider whose api binding is one of this crate's adapters.
///
/// Multi-api providers (github-copilot, opencode, fireworks, the Cloudflare
/// gateways) list their other apis too so the registry can bind them once the
/// sibling provider crates land.
pub fn openai_provider_descriptors() -> Vec<ProviderDescriptor> {
    use ProviderAuthKind as Auth;
    vec![
        // --- first party ---
        ProviderDescriptor::new(
            "openai",
            "OpenAI",
            Some("https://api.openai.com/v1"),
            &[RESPONSES],
            Auth::env("OpenAI API key", &["OPENAI_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "openai-codex",
            "OpenAI Codex",
            Some("https://chatgpt.com/backend-api"),
            &[CODEX],
            Auth::oauth_only("OpenAI (ChatGPT Plus/Pro)", true),
        ),
        ProviderDescriptor::new(
            "azure-openai-responses",
            "Azure OpenAI",
            None,
            &[AZURE],
            Auth::env("Azure OpenAI API key", &["AZURE_OPENAI_API_KEY"]),
        ),
        // --- OpenAI-compatible third parties ---
        ProviderDescriptor::new(
            "ant-ling",
            "Ant Ling",
            Some("https://api.ant-ling.com/v1"),
            &[COMPLETIONS],
            Auth::env("Ant Ling API key", &["ANT_LING_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "baseten",
            "Baseten",
            Some("https://inference.baseten.co/v1"),
            &[COMPLETIONS],
            Auth::env("Baseten API key", &["BASETEN_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "cerebras",
            "Cerebras",
            Some("https://api.cerebras.ai/v1"),
            &[COMPLETIONS],
            Auth::env("Cerebras API key", &["CEREBRAS_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "cloudflare-ai-gateway",
            "Cloudflare AI Gateway",
            None,
            &[ANTHROPIC, COMPLETIONS, RESPONSES],
            Auth::custom(),
        ),
        ProviderDescriptor::new(
            "cloudflare-workers-ai",
            "Cloudflare Workers AI",
            None,
            &[COMPLETIONS],
            Auth::custom(),
        ),
        ProviderDescriptor::new(
            "deepseek",
            "DeepSeek",
            Some("https://api.deepseek.com"),
            &[COMPLETIONS],
            Auth::env("DeepSeek API key", &["DEEPSEEK_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "fireworks",
            "Fireworks",
            Some("https://api.fireworks.ai/inference"),
            &[ANTHROPIC, COMPLETIONS],
            Auth::env("Fireworks API key", &["FIREWORKS_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "github-copilot",
            "GitHub Copilot",
            Some("https://api.individual.githubcopilot.com"),
            &[ANTHROPIC, COMPLETIONS, RESPONSES],
            Auth::env("GitHub Copilot token", &["COPILOT_GITHUB_TOKEN"])
                .with_oauth("GitHub Copilot", true),
        ),
        ProviderDescriptor::new(
            "groq",
            "Groq",
            Some("https://api.groq.com/openai/v1"),
            &[COMPLETIONS],
            Auth::env("Groq API key", &["GROQ_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "huggingface",
            "Hugging Face",
            Some("https://router.huggingface.co/v1"),
            &[COMPLETIONS],
            Auth::env("Hugging Face token", &["HF_TOKEN"]),
        ),
        ProviderDescriptor::new(
            "moonshotai",
            "Moonshot AI",
            Some("https://api.moonshot.ai/v1"),
            &[COMPLETIONS],
            Auth::env("Moonshot AI API key", &["MOONSHOT_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "moonshotai-cn",
            "Moonshot AI CN",
            Some("https://api.moonshot.cn/v1"),
            &[COMPLETIONS],
            Auth::env("Moonshot AI API key", &["MOONSHOT_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "nvidia",
            "NVIDIA",
            Some("https://integrate.api.nvidia.com/v1"),
            &[COMPLETIONS],
            Auth::env("NVIDIA API key", &["NVIDIA_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "opencode",
            "OpenCode Zen",
            None,
            &[ANTHROPIC, GOOGLE, COMPLETIONS, RESPONSES],
            Auth::env("OpenCode API key", &["OPENCODE_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "opencode-go",
            "OpenCode Go",
            None,
            &[ANTHROPIC, COMPLETIONS, RESPONSES],
            Auth::env("OpenCode API key", &["OPENCODE_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "openrouter",
            "OpenRouter",
            Some("https://openrouter.ai/api/v1"),
            &[COMPLETIONS],
            Auth::env("OpenRouter API key", &["OPENROUTER_API_KEY"])
                .with_oauth("OpenRouter OAuth", false),
        ),
        ProviderDescriptor::new(
            "qwen-token-plan",
            "Qwen Token Plan",
            Some("https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"),
            &[COMPLETIONS],
            Auth::env("Qwen Token Plan API key", &["QWEN_TOKEN_PLAN_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "qwen-token-plan-individual",
            "Qwen Token Plan Individual",
            Some("https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"),
            &[COMPLETIONS],
            Auth::env(
                "Qwen Token Plan Individual API key",
                &["QWEN_TOKEN_PLAN_API_KEY"],
            ),
        ),
        ProviderDescriptor::new(
            "qwen-token-plan-cn",
            "Qwen Token Plan CN",
            Some("https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"),
            &[COMPLETIONS],
            Auth::env(
                "Qwen Token Plan CN API key",
                &["QWEN_TOKEN_PLAN_CN_API_KEY"],
            ),
        ),
        ProviderDescriptor::new(
            "together",
            "Together",
            Some("https://api.together.ai/v1"),
            &[COMPLETIONS],
            Auth::env("Together API key", &["TOGETHER_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "xai",
            "xAI",
            Some("https://api.x.ai/v1"),
            &[RESPONSES],
            Auth::env("xAI API key", &["XAI_API_KEY"]).with_oauth("xAI", false),
        ),
        ProviderDescriptor::new(
            "xiaomi",
            "Xiaomi",
            Some("https://api.xiaomimimo.com/v1"),
            &[COMPLETIONS],
            Auth::env("Xiaomi API key", &["XIAOMI_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "xiaomi-token-plan-ams",
            "Xiaomi Token Plan AMS",
            Some("https://token-plan-ams.xiaomimimo.com/v1"),
            &[COMPLETIONS],
            Auth::env(
                "Xiaomi Token Plan AMS API key",
                &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
            ),
        ),
        ProviderDescriptor::new(
            "xiaomi-token-plan-cn",
            "Xiaomi Token Plan CN",
            Some("https://token-plan-cn.xiaomimimo.com/v1"),
            &[COMPLETIONS],
            Auth::env(
                "Xiaomi Token Plan CN API key",
                &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
            ),
        ),
        ProviderDescriptor::new(
            "xiaomi-token-plan-sgp",
            "Xiaomi Token Plan SGP",
            Some("https://token-plan-sgp.xiaomimimo.com/v1"),
            &[COMPLETIONS],
            Auth::env(
                "Xiaomi Token Plan SGP API key",
                &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
            ),
        ),
        ProviderDescriptor::new(
            "zai",
            "Z.AI",
            Some("https://api.z.ai/api/coding/paas/v4"),
            &[COMPLETIONS],
            Auth::env("Z.AI API key", &["ZAI_API_KEY"]),
        ),
        ProviderDescriptor::new(
            "zai-coding-cn",
            "Z.AI Coding CN",
            Some("https://open.bigmodel.cn/api/coding/paas/v4"),
            &[COMPLETIONS],
            Auth::env("Z.AI Coding CN API key", &["ZAI_CODING_CN_API_KEY"]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn descriptor_ids_are_unique_and_openai_shaped() {
        let descriptors = openai_provider_descriptors();
        let ids: HashSet<&str> = descriptors.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids.len(), descriptors.len());
        assert!(descriptors.iter().all(|d| d.is_openai_shaped()));
    }

    #[test]
    fn descriptors_round_trip_as_json() {
        let descriptors = openai_provider_descriptors();
        let json = serde_json::to_string(&descriptors).unwrap();
        let back: Vec<ProviderDescriptor> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, descriptors);
    }

    #[test]
    fn known_providers_carry_their_upstream_identity() {
        let descriptors = openai_provider_descriptors();
        let deepseek = descriptors.iter().find(|d| d.id == "deepseek").unwrap();
        assert_eq!(
            deepseek.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert_eq!(deepseek.apis, vec![crate::openai_completions::API]);
        assert_eq!(deepseek.auth.api_key_env, vec!["DEEPSEEK_API_KEY"]);

        let codex = descriptors.iter().find(|d| d.id == "openai-codex").unwrap();
        assert!(codex.auth.oauth);
        assert!(codex.auth.oauth_is_subscription);
        assert!(codex.auth.api_key_env.is_empty());
    }
}
