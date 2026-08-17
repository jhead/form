//! Port of `api/azure-openai-responses.ts`.
//!
//! Azure differs from plain Responses in three ways: the endpoint is a
//! deployment-scoped `/openai/v1` base with an `api-version` query parameter,
//! the model field carries the *deployment* name, and the credential is
//! mandatory (there is no "authorization header stands in for the key" path).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::event::{AssistantMessageEventSink, DoneReason, ErrorReason};
use pi_core::options::{ProviderEnv, SimpleStreamOptions, StreamOptions};
use pi_core::{
    AiError, ApiClient, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    Context, Model, StopReason,
};
use pi_http::HttpClient;
use serde_json::{json, Map, Value};

use crate::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::openai_responses::parse_level;
use crate::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, run_responses_sse,
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ResponsesStreamOptions,
    ResponsesStreamProcessor,
};
use crate::options::{provider_opt_str, ProviderOptionKey};
use crate::transport::{
    apply_on_payload, assign_all, base_json_sse_headers, finalize_headers, json_request,
    model_headers, post_sse_with_retry,
};
use crate::util::{format_provider_error, provider_env_value, MappedLevel};
use pi_provider_common::constrained_sampling::create_grammar_tool_input_properties;
use pi_provider_common::simple_options::build_base_options;

pub const API: &str = "azure-openai-responses";

const DEFAULT_AZURE_API_VERSION: &str = "v1";
const AZURE_TOOL_CALL_PROVIDERS: [&str; 4] = [
    "openai",
    "openai-codex",
    "opencode",
    "azure-openai-responses",
];
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// The `azure-openai-responses` [`ApiClient`].
#[derive(Clone)]
pub struct AzureOpenAiResponsesClient {
    http: Arc<HttpClient>,
}

impl Default for AzureOpenAiResponsesClient {
    fn default() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }
}

impl std::fmt::Debug for AzureOpenAiResponsesClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AzureOpenAiResponsesClient")
    }
}

impl AzureOpenAiResponsesClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl ApiClient for AzureOpenAiResponsesClient {
    fn api(&self) -> &str {
        API
    }

    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let (sink, stream) = AssistantMessageEventStream::channel(64);
        let http = self.http.clone();
        let model = model.clone();
        let context = context.clone();
        let options = options.clone();
        tokio::spawn(async move {
            run_stream(http, model, context, options, sink).await;
        });
        Ok(stream)
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        // A missing credential is reported as an in-stream `Error` event, not
        // as `Err`. `stream_inner` already checks the same condition on the
        // request path; upstream's `streamSimple` additionally throws
        // synchronously here, but the `ApiClient` contract in `pi-core`
        // reserves `Err` for programmer errors and an FFI caller should not
        // have two failure paths for one condition.
        let mut base = build_base_options(
            model,
            context,
            options,
            options.stream.request.api_key.as_deref(),
        );
        let clamped = options
            .reasoning
            .map(|level| model.clamp_thinking_level(level.into()));
        if let Some(effort) = clamped.and_then(|l| l.level()) {
            base.provider_options.insert(
                ProviderOptionKey::ReasoningEffort.as_str().to_string(),
                json!(effort.as_str()),
            );
        }
        self.stream(model, context, &base).await
    }
}

async fn run_stream(
    http: Arc<HttpClient>,
    model: Model,
    context: Context,
    options: StreamOptions,
    sink: AssistantMessageEventSink,
) {
    // Upstream hard-codes the api field rather than reading `model.api`.
    let mut output = AssistantMessage::pending(API, &model.provider, &model.id);

    match stream_inner(&http, &model, &context, &options, &mut output, &sink).await {
        Ok(reason) => {
            let _ = sink
                .send(AssistantMessageEvent::Done {
                    reason,
                    message: output,
                })
                .await;
        }
        Err(err) => {
            let aborted = options.request.is_aborted() || err.is_aborted();
            output.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            output.error_message =
                Some(format_provider_error(&err, Some("Azure OpenAI API error")));
            let _ = sink
                .send(AssistantMessageEvent::Error {
                    reason: if aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error: output,
                })
                .await;
        }
    }
}

async fn stream_inner(
    http: &HttpClient,
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventSink,
) -> Result<DoneReason, AiError> {
    let Some(api_key) = options.request.api_key.clone().filter(|k| !k.is_empty()) else {
        return Err(AiError::auth(format!(
            "No API key for provider: {}",
            model.provider
        )));
    };

    let deployment_name = resolve_deployment_name(model, options);
    let config = resolve_azure_config(model, options)?;
    let grammar_props = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        model
            .compat
            .as_ref()
            .and_then(|c| c.supports_openai_grammar_tools)
            .unwrap_or(false),
    )?;

    let mut headers = model_headers(model);
    assign_all(&mut headers, &options.request.headers);
    // Azure authenticates with `api-key`; the SDK also sends the bearer form.
    let mut defaults = base_json_sse_headers(&api_key);
    defaults.insert("api-key".into(), api_key.clone());
    let headers = finalize_headers(defaults, &headers);

    let body = build_azure_body(model, context, options, &deployment_name)?;
    let body = apply_on_payload(body, model, &options.request);

    let url = format!(
        "{}/responses?api-version={}",
        config.base_url, config.api_version
    );
    let request = json_request(url, body, headers, &options.request);
    let mut response = post_sse_with_retry(http, request, model, &options.request).await?;

    let _ = sink
        .send(AssistantMessageEvent::Start {
            partial: output.clone(),
        })
        .await;

    let mut processor = ResponsesStreamProcessor::new(ResponsesStreamOptions {
        service_tier: None,
        grammar_tool_input_properties: grammar_props,
        codex_service_tier_resolution: false,
        // Azure does not apply service-tier pricing.
        apply_service_tier_pricing: false,
    });

    run_responses_sse(
        &mut response,
        &options.request.signal,
        &mut processor,
        output,
        model,
        sink,
        None,
    )
    .await?;

    if options.request.is_aborted() {
        return Err(AiError::Aborted);
    }
    if output.stop_reason == StopReason::Pending {
        return Err(AiError::protocol(
            "Azure OpenAI Responses stream ended without a stop reason",
        ));
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        return Err(AiError::other(
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_string()),
        ));
    }

    Ok(match output.stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        _ => DoneReason::Stop,
    })
}

// ============================================================================
// Endpoint resolution
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureConfig {
    pub base_url: String,
    pub api_version: String,
}

/// Port of `parseDeploymentNameMap` + `resolveDeploymentName`.
pub fn resolve_deployment_name(model: &Model, options: &StreamOptions) -> String {
    if let Some(name) = provider_opt_str(options, ProviderOptionKey::AzureDeploymentName) {
        if !name.is_empty() {
            return name;
        }
    }
    let raw = provider_env_value("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", &options.request.env);
    if let Some(raw) = raw {
        for entry in raw.split(',') {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((model_id, deployment)) = trimmed.split_once('=') else {
                continue;
            };
            if model_id.trim() == model.id && !deployment.trim().is_empty() {
                return deployment.trim().to_string();
            }
        }
    }
    model.id.clone()
}

/// Port of `normalizeAzureBaseUrl`.
///
/// Azure hosts need `/openai/v1` as the base path so the deployment-scoped
/// request path and `api-version` land in the right places.
pub fn normalize_azure_base_url(base_url: &str) -> Result<String, AiError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let mut url = url::Url::parse(trimmed).map_err(|_| {
        AiError::invalid_request(format!("Invalid Azure OpenAI base URL: {base_url}"))
    })?;

    let host = url.host_str().unwrap_or_default().to_string();
    let is_azure_host = host.ends_with(".openai.azure.com")
        || host.ends_with(".cognitiveservices.azure.com")
        || host.ends_with(".ai.azure.com");
    let normalized_path = url.path().trim_end_matches('/').to_string();

    if is_azure_host
        && matches!(
            normalized_path.as_str(),
            "" | "/" | "/openai" | "/openai/v1/responses"
        )
    {
        url.set_path("/openai/v1");
        url.set_query(None);
    }

    Ok(url.to_string().trim_end_matches('/').to_string())
}

/// Port of `resolveAzureConfig`.
pub fn resolve_azure_config(
    model: &Model,
    options: &StreamOptions,
) -> Result<AzureConfig, AiError> {
    let env: &ProviderEnv = &options.request.env;
    let api_version = provider_opt_str(options, ProviderOptionKey::AzureApiVersion)
        .filter(|v| !v.is_empty())
        .or_else(|| provider_env_value("AZURE_OPENAI_API_VERSION", env))
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());

    let explicit_base = provider_opt_str(options, ProviderOptionKey::AzureBaseUrl)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| provider_env_value("AZURE_OPENAI_BASE_URL", env).map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty());
    let resource_name = provider_opt_str(options, ProviderOptionKey::AzureResourceName)
        .filter(|v| !v.is_empty())
        .or_else(|| provider_env_value("AZURE_OPENAI_RESOURCE_NAME", env));

    let resolved = explicit_base
        .or_else(|| resource_name.map(|name| format!("https://{name}.openai.azure.com/openai/v1")))
        .or_else(|| Some(model.base_url.clone()).filter(|b| !b.is_empty()));

    let Some(resolved) = resolved else {
        return Err(AiError::invalid_request(
            "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.",
        ));
    };

    Ok(AzureConfig {
        base_url: normalize_azure_base_url(&resolved)?,
        api_version,
    })
}

/// Port of `buildParams`.
pub fn build_azure_body(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    deployment_name: &str,
) -> Result<Value, AiError> {
    let supports_grammar = model
        .compat
        .as_ref()
        .and_then(|c| c.supports_openai_grammar_tools)
        .unwrap_or(false);
    let grammar_props =
        create_grammar_tool_input_properties(context.tools.as_deref(), supports_grammar)?;

    let messages = convert_responses_messages(
        model,
        context,
        &HashSet::from(AZURE_TOOL_CALL_PROVIDERS),
        &ConvertResponsesMessagesOptions {
            include_system_prompt: true,
            grammar_tool_input_properties: Some(&grammar_props),
            ..Default::default()
        },
    )?;

    let mut params = Map::new();
    params.insert("model".into(), json!(deployment_name));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), json!(true));
    if let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref()) {
        params.insert("prompt_cache_key".into(), json!(key));
    }
    params.insert("store".into(), json!(false));

    if let Some(max_tokens) = options.max_tokens.filter(|m| *m > 0) {
        params.insert(
            "max_output_tokens".into(),
            json!(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(temperature) = options.temperature {
        params.insert("temperature".into(), json!(temperature));
    }

    if !context.tools().is_empty() {
        // Azure defaults `supportsStrictMode` to true, unlike plain Responses.
        let tool_options = ConvertResponsesToolsOptions::new(
            model
                .compat
                .as_ref()
                .and_then(|c| c.supports_strict_mode)
                .unwrap_or(true),
            supports_grammar,
        );
        params.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(context.tools(), &tool_options)?),
        );
    }

    if model.reasoning {
        let effort = provider_opt_str(options, ProviderOptionKey::ReasoningEffort);
        let summary = provider_opt_str(options, ProviderOptionKey::ReasoningSummary);
        if effort.is_some() || summary.is_some() {
            let resolved = match effort.as_deref().and_then(parse_level) {
                Some(level) => MappedLevel::lookup(model, level.into()).or(level.as_str()),
                None => "medium".to_string(),
            };
            params.insert(
                "reasoning".into(),
                json!({
                    "effort": resolved,
                    "summary": summary.unwrap_or_else(|| "auto".to_string())
                }),
            );
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        } else {
            let off = MappedLevel::lookup(model, pi_core::model::ModelThinkingLevel::Off);
            if !off.is_null() {
                params.insert("reasoning".into(), json!({ "effort": off.or("none") }));
            }
        }
    }

    if let Some(sampling) = &options.sampling_params {
        for (key, value) in sampling {
            params.insert(key.clone(), value.clone());
        }
    }

    Ok(Value::Object(params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::Api;

    fn model(base_url: &str) -> Model {
        Model::new(
            "gpt-5",
            Api::AzureOpenAiResponses,
            "azure-openai-responses",
            base_url,
        )
    }

    #[test]
    fn azure_hosts_get_the_openai_v1_base_path() {
        assert_eq!(
            normalize_azure_base_url("https://acme.openai.azure.com").unwrap(),
            "https://acme.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://acme.openai.azure.com/openai/v1/responses").unwrap(),
            "https://acme.openai.azure.com/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://acme.cognitiveservices.azure.com/openai/").unwrap(),
            "https://acme.cognitiveservices.azure.com/openai/v1"
        );
    }

    #[test]
    fn non_azure_hosts_and_custom_paths_are_left_alone() {
        assert_eq!(
            normalize_azure_base_url("https://proxy.internal/gateway/openai/v1").unwrap(),
            "https://proxy.internal/gateway/openai/v1"
        );
        assert_eq!(
            normalize_azure_base_url("https://acme.openai.azure.com/custom/path").unwrap(),
            "https://acme.openai.azure.com/custom/path"
        );
    }

    #[test]
    fn deployment_name_map_is_read_from_the_env() {
        let mut options = StreamOptions::default();
        options.request.env.insert(
            "AZURE_OPENAI_DEPLOYMENT_NAME_MAP".into(),
            "gpt-4=prod-gpt4, gpt-5=prod-gpt5".into(),
        );
        assert_eq!(
            resolve_deployment_name(&model("https://x"), &options),
            "prod-gpt5"
        );
    }

    #[test]
    fn deployment_name_falls_back_to_the_model_id() {
        assert_eq!(
            resolve_deployment_name(&model("https://x"), &StreamOptions::default()),
            "gpt-5"
        );
    }

    #[test]
    fn missing_base_url_is_an_invalid_request() {
        let err = resolve_azure_config(&model(""), &StreamOptions::default()).unwrap_err();
        assert_eq!(err.code(), "invalid_request");
    }
}
