//! Provider descriptors.
//!
//! Port of `packages/ai/src/providers/*.ts` (the non-`.models.ts` files) plus
//! `providers/all.ts`.
//!
//! Upstream a `Provider` is a live object that owns both metadata *and* stream
//! behaviour (`createProvider` closes over an `api: ProviderStreams` map). That
//! shape cannot cross FFI and would force `pi-catalog` to depend on the
//! `pi-provider-*` crates. Here a [`Provider`] is pure data: it *declares* which
//! api ids its models use, and [`crate::ModelRegistry`] pairs those ids with
//! `Arc<dyn ApiClient>` adapters registered at runtime.
//!
//! Auth is likewise descriptive. Upstream's `ProviderAuth` carries `login` /
//! `resolve` closures; those flows are `pi-auth`'s (W4). What survives here is
//! the metadata W4 needs to build them: method kind, display name, and the
//! environment variables consulted in precedence order.

use std::collections::BTreeMap;

use pi_core::{Api, Model};
use serde::{Deserialize, Serialize};

use crate::model_catalog::builtin_models;

/// How a provider can be authenticated. Descriptive only — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKeyAuthInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthAuthInfo>,
}

impl ProviderAuth {
    fn env_key(name: &str, vars: &[&str]) -> Self {
        Self {
            api_key: Some(ApiKeyAuthInfo::new(name, vars)),
            oauth: None,
        }
    }

    fn with_oauth(mut self, oauth: OAuthAuthInfo) -> Self {
        self.oauth = Some(oauth);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyAuthInfo {
    /// Display name shown when prompting, e.g. "Anthropic API key".
    pub name: String,
    /// Environment variables consulted, in precedence order. Empty for
    /// providers whose credentials are ambient (AWS chain, gcloud ADC).
    #[serde(default)]
    pub env_vars: Vec<String>,
}

impl ApiKeyAuthInfo {
    fn new(name: &str, env_vars: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            env_vars: env_vars.iter().map(|v| v.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthAuthInfo {
    pub name: String,
    /// True when the flow grants a consumer subscription (Claude Pro/Max,
    /// ChatGPT Plus/Pro, ...) rather than metered API access.
    #[serde(default)]
    pub is_subscription: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_label: Option<String>,
}

impl OAuthAuthInfo {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            is_subscription: false,
            login_label: None,
        }
    }

    fn subscription(name: &str) -> Self {
        Self {
            name: name.to_string(),
            is_subscription: true,
            login_label: None,
        }
    }

    fn login_label(mut self, label: &str) -> Self {
        self.login_label = Some(label.to_string());
        self
    }
}

/// A provider: identity, endpoint, auth metadata, model list, api bindings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    /// Default endpoint. `None` for providers whose base URL is per-model or
    /// derived from account config (Bedrock, Azure, Cloudflare, OpenCode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    pub auth: ProviderAuth,
    /// Baseline catalog. Empty for purely dynamic providers, whose models
    /// arrive via [`crate::ModelRegistry::set_provider_models`].
    #[serde(default)]
    pub models: Vec<Model>,
    /// Api ids this provider's models use. The registry resolves each to a
    /// registered `ApiClient` at request time.
    #[serde(default)]
    pub apis: Vec<Api>,
    /// True when the catalog is fetched at runtime rather than embedded.
    #[serde(default)]
    pub dynamic: bool,
}

impl Provider {
    /// Minimal constructor for custom/user-defined providers.
    pub fn new(id: impl Into<String>, api: Api) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            base_url: None,
            headers: None,
            auth: ProviderAuth::default(),
            models: Vec::new(),
            apis: vec![api],
            dynamic: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn with_models(mut self, models: Vec<Model>) -> Self {
        self.models = models;
        self
    }

    pub fn with_auth(mut self, auth: ProviderAuth) -> Self {
        self.auth = auth;
        self
    }

    pub fn get_model(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Whether this provider declares an implementation for `api`.
    pub fn supports_api(&self, api: &Api) -> bool {
        self.apis.contains(api)
    }
}

/// Internal descriptor table row. Keeps [`builtin_providers`] readable.
struct Builtin {
    id: &'static str,
    name: &'static str,
    base_url: Option<&'static str>,
    apis: &'static [Api],
    auth: fn() -> ProviderAuth,
    dynamic: bool,
}

fn api_key(name: &'static str, vars: &'static [&'static str]) -> ProviderAuth {
    ProviderAuth::env_key(name, vars)
}

/// Every built-in provider, in upstream `builtinProviders()` order.
///
/// The auth column reproduces each provider's `envApiKeyAuth(...)` /
/// hand-written `ApiKeyAuth` from `providers/*.ts`. Providers with ambient
/// credential chains list the variables their upstream `resolve()` probes.
pub fn builtin_providers() -> Vec<Provider> {
    const BUILTINS: &[Builtin] = &[
        Builtin {
            id: "amazon-bedrock",
            name: "Amazon Bedrock",
            base_url: None,
            apis: &[Api::BedrockConverseStream],
            // Bedrock resolves through the AWS credential chain; the bearer
            // token short-circuits it.
            auth: || {
                api_key(
                    "AWS credentials or bearer token",
                    &[
                        "AWS_BEARER_TOKEN_BEDROCK",
                        "AWS_PROFILE",
                        "AWS_ACCESS_KEY_ID",
                        "AWS_SECRET_ACCESS_KEY",
                        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
                        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
                        "AWS_WEB_IDENTITY_TOKEN_FILE",
                    ],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "ant-ling",
            name: "Ant Ling",
            base_url: Some("https://api.ant-ling.com/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Ant Ling API key", &["ANT_LING_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "anthropic",
            name: "Anthropic",
            base_url: Some("https://api.anthropic.com"),
            apis: &[Api::AnthropicMessages],
            // ANTHROPIC_AUTH_TOKEN wins and is sent as `Authorization: Bearer`;
            // the other two are plain api keys.
            auth: || {
                api_key(
                    "Anthropic API key",
                    &[
                        "ANTHROPIC_AUTH_TOKEN",
                        "ANTHROPIC_OAUTH_TOKEN",
                        "ANTHROPIC_API_KEY",
                    ],
                )
                .with_oauth(OAuthAuthInfo::subscription("Anthropic (Claude Pro/Max)"))
            },
            dynamic: false,
        },
        Builtin {
            id: "azure-openai-responses",
            name: "Azure OpenAI",
            base_url: None,
            apis: &[Api::AzureOpenAiResponses],
            auth: || api_key("Azure OpenAI API key", &["AZURE_OPENAI_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "baseten",
            name: "Baseten",
            base_url: Some("https://inference.baseten.co/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Baseten API key", &["BASETEN_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "cerebras",
            name: "Cerebras",
            base_url: Some("https://api.cerebras.ai/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Cerebras API key", &["CEREBRAS_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "cloudflare-ai-gateway",
            name: "Cloudflare AI Gateway",
            base_url: None,
            apis: &[
                Api::AnthropicMessages,
                Api::OpenAiCompletions,
                Api::OpenAiResponses,
            ],
            auth: || {
                api_key(
                    "Cloudflare API key",
                    &[
                        "CLOUDFLARE_API_KEY",
                        "CLOUDFLARE_ACCOUNT_ID",
                        "CLOUDFLARE_GATEWAY_ID",
                    ],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "cloudflare-workers-ai",
            name: "Cloudflare Workers AI",
            base_url: None,
            apis: &[Api::OpenAiCompletions],
            auth: || {
                api_key(
                    "Cloudflare API key",
                    &["CLOUDFLARE_API_KEY", "CLOUDFLARE_ACCOUNT_ID"],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "deepseek",
            name: "DeepSeek",
            base_url: Some("https://api.deepseek.com"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("DeepSeek API key", &["DEEPSEEK_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "fireworks",
            name: "Fireworks",
            base_url: Some("https://api.fireworks.ai/inference"),
            apis: &[Api::AnthropicMessages, Api::OpenAiCompletions],
            auth: || api_key("Fireworks API key", &["FIREWORKS_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "github-copilot",
            name: "GitHub Copilot",
            base_url: Some("https://api.individual.githubcopilot.com"),
            apis: &[
                Api::AnthropicMessages,
                Api::OpenAiCompletions,
                Api::OpenAiResponses,
            ],
            auth: || {
                api_key("GitHub Copilot token", &["COPILOT_GITHUB_TOKEN"])
                    .with_oauth(OAuthAuthInfo::subscription("GitHub Copilot"))
            },
            dynamic: false,
        },
        Builtin {
            id: "google",
            name: "Google",
            base_url: Some("https://generativelanguage.googleapis.com/v1beta"),
            apis: &[Api::GoogleGenerativeAi],
            auth: || api_key("Gemini API key", &["GEMINI_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "google-vertex",
            name: "Google Vertex AI",
            base_url: None,
            apis: &[Api::GoogleVertex],
            // An explicit key wins; otherwise ADC needs a credentials file plus
            // project and location.
            auth: || {
                api_key(
                    "Google Cloud credentials",
                    &[
                        "GOOGLE_CLOUD_API_KEY",
                        "GOOGLE_APPLICATION_CREDENTIALS",
                        "GOOGLE_CLOUD_PROJECT",
                        "GCLOUD_PROJECT",
                        "GOOGLE_CLOUD_LOCATION",
                    ],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "groq",
            name: "Groq",
            base_url: Some("https://api.groq.com/openai/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Groq API key", &["GROQ_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "huggingface",
            name: "Hugging Face",
            base_url: Some("https://router.huggingface.co/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Hugging Face token", &["HF_TOKEN"]),
            dynamic: false,
        },
        Builtin {
            id: "kimi-coding",
            name: "Kimi For Coding",
            base_url: Some("https://api.kimi.com/coding"),
            apis: &[Api::AnthropicMessages],
            auth: || {
                api_key("Kimi API key", &["KIMI_API_KEY"]).with_oauth(
                    OAuthAuthInfo::subscription("Kimi Code (subscription)")
                        .login_label("Sign in with Kimi Code"),
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "minimax",
            name: "MiniMax",
            base_url: Some("https://api.minimax.io/anthropic"),
            apis: &[Api::AnthropicMessages],
            auth: || api_key("MiniMax API key", &["MINIMAX_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "minimax-cn",
            name: "MiniMax CN",
            base_url: Some("https://api.minimaxi.com/anthropic"),
            apis: &[Api::AnthropicMessages],
            auth: || api_key("MiniMax CN API key", &["MINIMAX_CN_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "mistral",
            name: "Mistral",
            base_url: Some("https://api.mistral.ai"),
            apis: &[Api::MistralConversations],
            auth: || api_key("Mistral API key", &["MISTRAL_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "moonshotai",
            name: "Moonshot AI",
            base_url: Some("https://api.moonshot.ai/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Moonshot AI API key", &["MOONSHOT_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "moonshotai-cn",
            name: "Moonshot AI CN",
            base_url: Some("https://api.moonshot.cn/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Moonshot AI API key", &["MOONSHOT_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "nvidia",
            name: "NVIDIA",
            base_url: Some("https://integrate.api.nvidia.com/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("NVIDIA API key", &["NVIDIA_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "openai",
            name: "OpenAI",
            base_url: Some("https://api.openai.com/v1"),
            apis: &[Api::OpenAiResponses],
            auth: || api_key("OpenAI API key", &["OPENAI_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "openai-codex",
            name: "OpenAI Codex",
            base_url: Some("https://chatgpt.com/backend-api"),
            apis: &[Api::OpenAiCodexResponses],
            // OAuth only: there is no api-key path for the Codex backend.
            auth: || ProviderAuth {
                api_key: None,
                oauth: Some(OAuthAuthInfo::subscription("OpenAI (ChatGPT Plus/Pro)")),
            },
            dynamic: false,
        },
        Builtin {
            id: "opencode",
            name: "OpenCode Zen",
            base_url: None,
            apis: &[
                Api::AnthropicMessages,
                Api::GoogleGenerativeAi,
                Api::OpenAiCompletions,
                Api::OpenAiResponses,
            ],
            auth: || api_key("OpenCode API key", &["OPENCODE_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "opencode-go",
            name: "OpenCode Go",
            base_url: None,
            apis: &[
                Api::AnthropicMessages,
                Api::OpenAiCompletions,
                Api::OpenAiResponses,
            ],
            auth: || api_key("OpenCode API key", &["OPENCODE_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "openrouter",
            name: "OpenRouter",
            base_url: Some("https://openrouter.ai/api/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || {
                api_key("OpenRouter API key", &["OPENROUTER_API_KEY"]).with_oauth(
                    OAuthAuthInfo::new("OpenRouter OAuth").login_label("Sign in with OpenRouter"),
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "qwen-token-plan",
            name: "Qwen Token Plan",
            base_url: Some(
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
            ),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Qwen Token Plan API key", &["QWEN_TOKEN_PLAN_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "qwen-token-plan-cn",
            name: "Qwen Token Plan CN",
            base_url: Some("https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || {
                api_key(
                    "Qwen Token Plan CN API key",
                    &["QWEN_TOKEN_PLAN_CN_API_KEY"],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "qwen-token-plan-individual",
            name: "Qwen Token Plan Individual",
            base_url: Some(
                "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
            ),
            apis: &[Api::OpenAiCompletions],
            auth: || {
                api_key(
                    "Qwen Token Plan Individual API key",
                    &["QWEN_TOKEN_PLAN_API_KEY"],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "radius",
            name: "Radius",
            base_url: None,
            apis: &[Api::PiMessages],
            auth: || {
                api_key("Radius API key", &["RADIUS_API_KEY"])
                    .with_oauth(OAuthAuthInfo::new("Radius"))
            },
            // Catalog comes from the gateway's /v1/config, never embedded.
            dynamic: true,
        },
        Builtin {
            id: "together",
            name: "Together",
            base_url: Some("https://api.together.ai/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Together API key", &["TOGETHER_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "vercel-ai-gateway",
            name: "Vercel AI Gateway",
            base_url: Some("https://ai-gateway.vercel.sh"),
            apis: &[Api::AnthropicMessages],
            auth: || api_key("Vercel AI Gateway API key", &["AI_GATEWAY_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "xai",
            name: "xAI",
            base_url: Some("https://api.x.ai/v1"),
            apis: &[Api::OpenAiResponses],
            auth: || {
                api_key("xAI API key", &["XAI_API_KEY"]).with_oauth(
                    OAuthAuthInfo::subscription("xAI (Grok/X subscription)")
                        .login_label("Sign in with SuperGrok or X Premium"),
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "xiaomi",
            name: "Xiaomi",
            base_url: Some("https://api.xiaomimimo.com/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Xiaomi API key", &["XIAOMI_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "xiaomi-token-plan-ams",
            name: "Xiaomi Token Plan AMS",
            base_url: Some("https://token-plan-ams.xiaomimimo.com/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || {
                api_key(
                    "Xiaomi Token Plan AMS API key",
                    &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "xiaomi-token-plan-cn",
            name: "Xiaomi Token Plan CN",
            base_url: Some("https://token-plan-cn.xiaomimimo.com/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || {
                api_key(
                    "Xiaomi Token Plan CN API key",
                    &["XIAOMI_TOKEN_PLAN_CN_API_KEY"],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "xiaomi-token-plan-sgp",
            name: "Xiaomi Token Plan SGP",
            base_url: Some("https://token-plan-sgp.xiaomimimo.com/v1"),
            apis: &[Api::OpenAiCompletions],
            auth: || {
                api_key(
                    "Xiaomi Token Plan SGP API key",
                    &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"],
                )
            },
            dynamic: false,
        },
        Builtin {
            id: "zai",
            name: "Z.AI",
            base_url: Some("https://api.z.ai/api/coding/paas/v4"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Z.AI API key", &["ZAI_API_KEY"]),
            dynamic: false,
        },
        Builtin {
            id: "zai-coding-cn",
            name: "Z.AI Coding CN",
            base_url: Some("https://open.bigmodel.cn/api/coding/paas/v4"),
            apis: &[Api::OpenAiCompletions],
            auth: || api_key("Z.AI Coding CN API key", &["ZAI_CODING_CN_API_KEY"]),
            dynamic: false,
        },
    ];

    BUILTINS
        .iter()
        .map(|builtin| Provider {
            id: builtin.id.to_string(),
            name: builtin.name.to_string(),
            base_url: builtin.base_url.map(str::to_string),
            headers: None,
            auth: (builtin.auth)(),
            models: builtin_models(builtin.id),
            apis: builtin.apis.to_vec(),
            dynamic: builtin.dynamic,
        })
        .collect()
}

/// Look up one built-in provider descriptor by id.
pub fn builtin_provider(id: &str) -> Option<Provider> {
    builtin_providers().into_iter().find(|p| p.id == id)
}
