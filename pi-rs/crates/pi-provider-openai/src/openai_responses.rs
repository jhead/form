//! Port of `api/openai-responses.ts`.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::event::{AssistantMessageEventSink, DoneReason, ErrorReason};
use pi_core::model::{SessionAffinityFormat, ThinkingLevel};
use pi_core::options::{ProviderHeaders, SimpleStreamOptions, StreamOptions};
use pi_core::{
    AiError, ApiClient, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    CacheRetention, Context, Model, StopReason,
};
use pi_http::HttpClient;
use serde_json::{json, Map, Value};

use crate::compat::{resolve_cache_retention, responses_compat, ResponsesCompat};
use crate::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, run_responses_sse,
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ResponsesStreamOptions,
    ResponsesStreamProcessor,
};
use crate::options::{provider_opt_str, provider_opt_value, ProviderOptionKey};
use crate::transport::{
    apply_on_payload, assign_all, assign_header, base_json_sse_headers, client_api_key,
    finalize_headers, join_url, json_request, model_headers, post_sse_with_retry,
};
use crate::util::{force_pi_user_agent, format_provider_error, MappedLevel};
use pi_provider_common::constrained_sampling::create_grammar_tool_input_properties;
use pi_provider_common::simple_options::build_base_options;

pub const API: &str = "openai-responses";

const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
/// OpenAI Responses rejects `max_output_tokens` below 16.
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// The `openai-responses` [`ApiClient`].
#[derive(Clone)]
pub struct OpenAiResponsesClient {
    http: Arc<HttpClient>,
}

impl Default for OpenAiResponsesClient {
    fn default() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }
}

impl std::fmt::Debug for OpenAiResponsesClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenAiResponsesClient")
    }
}

impl OpenAiResponsesClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }
}

#[async_trait]
impl ApiClient for OpenAiResponsesClient {
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
    let mut output = AssistantMessage::pending(model.api.as_str(), &model.provider, &model.id);

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
            output.error_message = Some(format_provider_error(&err, Some("OpenAI API error")));
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
    let api_key = client_api_key(
        &model.provider,
        options.request.api_key.as_deref(),
        &options.request.headers,
    )?;
    let cache_retention = resolve_cache_retention(options.cache_retention, &options.request.env);
    let cache_session_id = match cache_retention {
        CacheRetention::None => None,
        _ => options.session_id.clone(),
    };
    let compat = responses_compat(model);
    let grammar_props = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools,
    )?;

    let headers = build_headers(
        model,
        context,
        &api_key,
        &options.request.headers,
        cache_session_id.as_deref(),
        &compat,
    );
    let body = build_responses_body(model, context, options, &compat, cache_retention)?;
    let body = apply_on_payload(body, model, &options.request);

    let url = join_url(&model.base_url, "responses");
    let request = json_request(url, body, headers, &options.request);
    let mut response = post_sse_with_retry(http, request, model, &options.request).await?;

    let _ = sink
        .send(AssistantMessageEvent::Start {
            partial: output.clone(),
        })
        .await;

    let mut processor = ResponsesStreamProcessor::new(ResponsesStreamOptions {
        service_tier: provider_opt_str(options, ProviderOptionKey::ServiceTier),
        grammar_tool_input_properties: grammar_props,
        codex_service_tier_resolution: false,
        apply_service_tier_pricing: true,
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
            "OpenAI Responses stream ended without a stop reason",
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

fn build_headers(
    model: &Model,
    context: &Context,
    api_key: &str,
    option_headers: &ProviderHeaders,
    session_id: Option<&str>,
    compat: &ResponsesCompat,
) -> pi_http::HeaderMap {
    let mut headers = model_headers(model);

    if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        for (name, value) in build_copilot_dynamic_headers(&context.messages, has_images) {
            assign_header(&mut headers, &name, Some(value));
        }
    }

    // Unlike completions, Responses does not gate affinity on a compat flag.
    if let Some(session_id) = session_id {
        match compat.session_affinity_format {
            SessionAffinityFormat::Openrouter => {
                assign_header(&mut headers, "x-session-id", Some(session_id.to_string()));
            }
            format => {
                if format == SessionAffinityFormat::Openai {
                    assign_header(&mut headers, "session_id", Some(session_id.to_string()));
                }
                assign_header(
                    &mut headers,
                    "x-client-request-id",
                    Some(session_id.to_string()),
                );
            }
        }
    }

    assign_all(&mut headers, option_headers);

    if model.provider == "xai" {
        force_pi_user_agent(&mut headers);
    }

    finalize_headers(base_json_sse_headers(api_key), &headers)
}

/// Port of `buildParams`.
pub fn build_responses_body(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    compat: &ResponsesCompat,
    cache_retention: CacheRetention,
) -> Result<Value, AiError> {
    let grammar_props = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools,
    )?;
    let deferred_tools_mode = compat.deferred_tools_mode();
    let (immediate, deferred) = crate::openai_responses_shared::split_deferred_tools(
        context,
        deferred_tools_mode.is_some(),
    );

    let tool_options = ConvertResponsesToolsOptions::new(
        compat.supports_strict_mode,
        compat.supports_openai_grammar_tools,
    );
    let messages = convert_responses_messages(
        model,
        context,
        &HashSet::from(OPENAI_TOOL_CALL_PROVIDERS),
        &ConvertResponsesMessagesOptions {
            include_system_prompt: true,
            grammar_tool_input_properties: Some(&grammar_props),
            deferred_tools: Some(&deferred),
            deferred_tools_mode,
            tool_options: tool_options.clone(),
        },
    )?;

    let mut params = Map::new();
    params.insert("model".into(), json!(model.id));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), json!(true));
    if cache_retention != CacheRetention::None {
        if let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref()) {
            params.insert("prompt_cache_key".into(), json!(key));
        }
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert("prompt_cache_retention".into(), json!("24h"));
    }
    // `cacheRetention: "none"` on a model that understands explicit mode turns
    // implicit prompt caching off outright rather than just skipping the key.
    if cache_retention == CacheRetention::None && compat.supports_explicit_prompt_cache_mode {
        params.insert("prompt_cache_options".into(), json!({ "mode": "explicit" }));
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
    if let Some(tier) = provider_opt_value(options, ProviderOptionKey::ServiceTier) {
        params.insert("service_tier".into(), tier);
    }
    if !immediate.is_empty() {
        params.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(&immediate, &tool_options)?),
        );
    }
    if let Some(tool_choice) = provider_opt_value(options, ProviderOptionKey::ToolChoice) {
        params.insert("tool_choice".into(), tool_choice);
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
        } else if model.provider != "github-copilot" {
            let off = MappedLevel::lookup(model, pi_core::model::ModelThinkingLevel::Off);
            if !off.is_null() {
                params.insert("reasoning".into(), json!({ "effort": off.or("none") }));
            }
        }
        if model.provider == "xai" {
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
    }

    // Last, so caller keys override the named request fields.
    if let Some(sampling) = &options.sampling_params {
        for (key, value) in sampling {
            params.insert(key.clone(), value.clone());
        }
    }

    Ok(Value::Object(params))
}

pub(crate) fn parse_level(value: &str) -> Option<ThinkingLevel> {
    Some(match value {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => return None,
    })
}

/// Build the request body with compat and cache retention resolved from options.
pub fn build_body_for(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
) -> Result<Value, AiError> {
    let compat = responses_compat(model);
    let cache_retention = resolve_cache_retention(options.cache_retention, &options.request.env);
    build_responses_body(model, context, options, &compat, cache_retention)
}
