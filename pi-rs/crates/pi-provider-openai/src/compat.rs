//! Resolved compatibility matrices.
//!
//! Port of `detectCompat` / `getCompat` from `api/openai-completions.ts` and the
//! `getCompat` in `api/openai-responses.ts`. `ModelCompat` in `pi-core` is the
//! sparse, optional form that ships in the catalog; the structs here are the
//! fully-resolved form the adapters branch on, with provider/base-URL detection
//! filled in underneath any explicit override.

use pi_core::model::{MaxTokensField, SessionAffinityFormat, ThinkingFormat};
use pi_core::{CacheRetention, Model};
use serde_json::{Map, Value};

/// Fully resolved `openai-completions` compat flags.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionsCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_finish_reason: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: ThinkingFormat,
    pub open_router_routing: Option<Value>,
    pub vercel_gateway_routing: Option<Value>,
    pub chat_template_kwargs: Map<String, Value>,
    pub chat_template_args: Map<String, Value>,
    pub zai_tool_stream: bool,
    pub supports_thinking_token_budget: bool,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    /// Only `"anthropic"` is meaningful today (OpenRouter → Anthropic models).
    pub cache_control_format: Option<String>,
    pub send_session_affinity_headers: bool,
    /// Only `"kimi"` is meaningful today.
    pub deferred_tools_mode: Option<String>,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
}

/// Port of `detectCompat`.
pub fn detect_completions_compat(model: &Model) -> CompletionsCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();
    let lower_base = base_url.to_lowercase();

    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_openrouter = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_deepseek = provider == "deepseek" || lower_base.contains("deepseek.com");

    let is_non_standard = is_nvidia
        || provider == "cerebras"
        || base_url.contains("cerebras.ai")
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;

    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;

    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_openrouter_developer_role_model =
        is_openrouter && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    let cache_control_format = if provider == "openrouter" && model.id.starts_with("anthropic/") {
        Some("anthropic".to_string())
    } else {
        None
    };

    CompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: is_openrouter_developer_role_model
            || (!is_non_standard && !is_openrouter),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format: if is_deepseek {
            ThinkingFormat::Deepseek
        } else if is_zai {
            ThinkingFormat::Zai
        } else if is_together {
            ThinkingFormat::Together
        } else if is_ant_ling {
            ThinkingFormat::AntLing
        } else if is_openrouter {
            ThinkingFormat::Openrouter
        } else {
            ThinkingFormat::Openai
        },
        open_router_routing: None,
        vercel_gateway_routing: None,
        chat_template_kwargs: Map::new(),
        chat_template_args: Map::new(),
        zai_tool_stream: false,
        supports_thinking_token_budget: false,
        supports_strict_mode: !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia,
        supports_openai_grammar_tools: false,
        cache_control_format,
        send_session_affinity_headers: false,
        deferred_tools_mode: None,
        session_affinity_format: if is_openrouter {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
    }
}

/// Port of `getCompat`: detection first, then explicit `model.compat` overrides.
pub fn completions_compat(model: &Model) -> CompletionsCompat {
    let detected = detect_completions_compat(model);
    let Some(compat) = &model.compat else {
        return detected;
    };

    CompletionsCompat {
        supports_store: compat.supports_store.unwrap_or(detected.supports_store),
        supports_developer_role: compat
            .supports_developer_role
            .unwrap_or(detected.supports_developer_role),
        supports_reasoning_effort: compat
            .supports_reasoning_effort
            .unwrap_or(detected.supports_reasoning_effort),
        supports_usage_in_streaming: compat
            .supports_usage_in_streaming
            .unwrap_or(detected.supports_usage_in_streaming),
        supports_finish_reason: compat
            .supports_finish_reason
            .unwrap_or(detected.supports_finish_reason),
        max_tokens_field: compat.max_tokens_field.unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: compat
            .requires_tool_result_name
            .unwrap_or(detected.requires_tool_result_name),
        requires_assistant_after_tool_result: compat
            .requires_assistant_after_tool_result
            .unwrap_or(detected.requires_assistant_after_tool_result),
        requires_thinking_as_text: compat
            .requires_thinking_as_text
            .unwrap_or(detected.requires_thinking_as_text),
        requires_reasoning_content_on_assistant_messages: compat
            .requires_reasoning_content_on_assistant_messages
            .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
        thinking_format: compat.thinking_format.unwrap_or(detected.thinking_format),
        // Upstream deliberately falls back to `{}` here rather than the detected
        // value, so an explicit compat block clears any inherited routing.
        open_router_routing: compat.open_router_routing.clone(),
        vercel_gateway_routing: compat
            .vercel_gateway_routing
            .clone()
            .or_else(|| detected.vercel_gateway_routing.clone()),
        chat_template_kwargs: compat
            .chat_template_kwargs
            .clone()
            .unwrap_or_else(|| detected.chat_template_kwargs.clone()),
        chat_template_args: compat
            .chat_template_args
            .clone()
            .unwrap_or_else(|| detected.chat_template_args.clone()),
        zai_tool_stream: compat.zai_tool_stream.unwrap_or(detected.zai_tool_stream),
        supports_thinking_token_budget: compat
            .supports_thinking_token_budget
            .unwrap_or(detected.supports_thinking_token_budget),
        supports_strict_mode: compat
            .supports_strict_mode
            .unwrap_or(detected.supports_strict_mode),
        supports_openai_grammar_tools: compat
            .supports_openai_grammar_tools
            .unwrap_or(detected.supports_openai_grammar_tools),
        cache_control_format: compat
            .cache_control_format
            .clone()
            .or_else(|| detected.cache_control_format.clone()),
        send_session_affinity_headers: compat
            .send_session_affinity_headers
            .unwrap_or(detected.send_session_affinity_headers),
        deferred_tools_mode: compat
            .deferred_tools_mode
            .clone()
            .or_else(|| detected.deferred_tools_mode.clone()),
        session_affinity_format: compat
            .session_affinity_format
            .unwrap_or(detected.session_affinity_format),
        supports_long_cache_retention: compat
            .supports_long_cache_retention
            .unwrap_or(detected.supports_long_cache_retention),
    }
}

/// Fully resolved `openai-responses` compat flags.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponsesCompat {
    pub supports_developer_role: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub supports_additional_tools: bool,
    pub supports_tool_search: bool,
    pub supports_explicit_prompt_cache_mode: bool,
}

/// Port of `openai-responses.ts#getCompat`.
pub fn responses_compat(model: &Model) -> ResponsesCompat {
    let detected_affinity =
        if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        };
    let compat = model.compat.as_ref();
    ResponsesCompat {
        supports_developer_role: compat
            .and_then(|c| c.supports_developer_role)
            .unwrap_or(true),
        session_affinity_format: compat
            .and_then(|c| c.session_affinity_format)
            .unwrap_or(detected_affinity),
        supports_long_cache_retention: compat
            .and_then(|c| c.supports_long_cache_retention)
            .unwrap_or(true),
        supports_strict_mode: compat.and_then(|c| c.supports_strict_mode).unwrap_or(false),
        supports_openai_grammar_tools: compat
            .and_then(|c| c.supports_openai_grammar_tools)
            .unwrap_or(false),
        supports_additional_tools: compat
            .and_then(|c| c.supports_additional_tools)
            .unwrap_or(false),
        supports_tool_search: compat.and_then(|c| c.supports_tool_search).unwrap_or(false),
        supports_explicit_prompt_cache_mode: compat
            .and_then(|c| c.supports_explicit_prompt_cache_mode)
            .unwrap_or(false),
    }
}

/// Deferred-tool placement for the Responses family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredToolsMode {
    AdditionalTools,
    ToolSearch,
}

impl ResponsesCompat {
    pub fn deferred_tools_mode(&self) -> Option<DeferredToolsMode> {
        if self.supports_additional_tools {
            Some(DeferredToolsMode::AdditionalTools)
        } else if self.supports_tool_search {
            Some(DeferredToolsMode::ToolSearch)
        } else {
            None
        }
    }
}

/// Port of `resolveCacheRetention`: explicit option, then `PI_CACHE_RETENTION`,
/// then `short`.
pub fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: &pi_core::options::ProviderEnv,
) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if crate::util::provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::model::ModelCompat;
    use pi_core::Api;

    fn model(provider: &str, base_url: &str) -> Model {
        Model::new("m", Api::OpenAiCompletions, provider, base_url)
    }

    #[test]
    fn deepseek_is_detected_from_the_base_url_alone() {
        let compat = detect_completions_compat(&model("custom", "https://api.DEEPSEEK.com/v1"));
        assert_eq!(compat.thinking_format, ThinkingFormat::Deepseek);
        assert_eq!(compat.max_tokens_field, MaxTokensField::MaxTokens);
        assert!(compat.requires_reasoning_content_on_assistant_messages);
        assert!(!compat.supports_store);
    }

    #[test]
    fn openrouter_developer_role_only_for_anthropic_and_openai_models() {
        let mut m = model("openrouter", "https://openrouter.ai/api/v1");
        m.id = "anthropic/claude".into();
        assert!(detect_completions_compat(&m).supports_developer_role);
        assert_eq!(
            detect_completions_compat(&m)
                .cache_control_format
                .as_deref(),
            Some("anthropic")
        );
        m.id = "meta/llama".into();
        assert!(!detect_completions_compat(&m).supports_developer_role);
        assert_eq!(detect_completions_compat(&m).cache_control_format, None);
    }

    #[test]
    fn explicit_compat_overrides_detection() {
        let mut m = model("deepseek", "https://api.deepseek.com");
        m.compat = Some(ModelCompat {
            max_tokens_field: Some(MaxTokensField::MaxCompletionTokens),
            thinking_format: Some(ThinkingFormat::Qwen),
            ..Default::default()
        });
        let compat = completions_compat(&m);
        assert_eq!(compat.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        assert_eq!(compat.thinking_format, ThinkingFormat::Qwen);
        // Untouched keys still come from detection.
        assert!(compat.requires_reasoning_content_on_assistant_messages);
    }

    #[test]
    fn together_disables_strict_mode_and_long_cache() {
        let compat = detect_completions_compat(&model("together", "https://api.together.ai/v1"));
        assert!(!compat.supports_strict_mode);
        assert!(!compat.supports_long_cache_retention);
        assert!(!compat.supports_reasoning_effort);
        assert_eq!(compat.thinking_format, ThinkingFormat::Together);
    }
}
