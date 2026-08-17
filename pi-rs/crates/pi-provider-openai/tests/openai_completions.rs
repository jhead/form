//! `openai-completions` adapter, end to end against `wiremock`.
//!
//! Assertions are on the **full event sequence**, not just the final message:
//! upstream's contract is the ordering and the `contentIndex` numbering, and
//! those are what a port gets wrong.

mod common;

use common::*;
use pi_core::model::{MaxTokensField, ModelThinkingLevel, SessionAffinityFormat, ThinkingFormat};
use pi_core::tool::{ConstrainedSampling, ConstrainedSamplingConfig, GrammarVariants, StrictMode};
use pi_core::{
    Api, ApiClient, AssistantContent, CacheRetention, Context, Message, StopReason, Tool,
};
use pi_provider_openai::openai_completions::build_body_for;
use pi_provider_openai::options::ProviderOptionKey;
use pi_provider_openai::OpenAiCompletionsClient;
use pretty_assertions::assert_eq;
use serde_json::json;

const ENDPOINT: &str = "/chat/completions";

// ============================================================================
// Streaming: event sequences
// ============================================================================

#[tokio::test]
async fn text_stream_emits_the_full_event_sequence() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let model = completions_model(&provider.base_url());
    let client = OpenAiCompletionsClient::new();

    let stream = client
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::TextStart(0),
            Ev::TextDelta(0, "Hello".into()),
            Ev::TextDelta(0, ", world".into()),
            Ev::TextEnd(0, "Hello, world".into()),
            Ev::Done("stop".into()),
        ]
    );

    let message = &collected.terminal;
    assert_eq!(message.api, "openai-completions");
    assert_eq!(message.provider, "openai");
    assert_eq!(message.text(), "Hello, world");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("chatcmpl-1"));
    assert_eq!(message.raw_stop_reason.as_deref(), Some("stop"));
}

#[tokio::test]
async fn partial_snapshots_grow_monotonically() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    // start: empty; text_start: one empty block; then the text accumulates.
    assert_eq!(collected.partial_at(0).content.len(), 0);
    assert_eq!(collected.partial_at(1).text(), "");
    assert_eq!(collected.partial_at(2).text(), "Hello");
    assert_eq!(collected.partial_at(3).text(), "Hello, world");
    assert_eq!(collected.partial_at(4).text(), "Hello, world");

    // Partials stay `pending` until the chunk carrying `finish_reason` is read.
    // The trailing `text_end` is emitted afterwards, so it already sees `stop` —
    // upstream behaves the same way, because `finishBlock` runs after the loop.
    for index in 0..=3 {
        assert_eq!(collected.partial_at(index).stop_reason, StopReason::Pending);
    }
    assert_eq!(collected.partial_at(4).stop_reason, StopReason::Stop);

    // Usage only lands on the final chunk, so earlier partials report zero.
    assert_eq!(collected.partial_at(3).usage.total_tokens, 0);
    assert_eq!(collected.terminal.usage.total_tokens, 128);
}

#[tokio::test]
async fn usage_and_cost_account_for_cache_reads_and_reasoning() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let usage = collect(stream).await.terminal.usage;

    // prompt_tokens 120 with 20 cached reads → 100 billable input.
    assert_eq!(usage.input, 100);
    assert_eq!(usage.output, 8);
    assert_eq!(usage.cache_read, 20);
    assert_eq!(usage.cache_write, 0);
    assert_eq!(usage.reasoning, Some(3));
    assert_eq!(usage.total_tokens, 128);
    // 100 * 1.0/1e6 + 8 * 2.0/1e6 + 20 * 0.5/1e6
    assert!((usage.cost.input - 0.0001).abs() < 1e-12);
    assert!((usage.cost.output - 0.000016).abs() < 1e-12);
    assert!((usage.cost.cache_read - 0.00001).abs() < 1e-12);
    assert!((usage.cost.total - 0.000126).abs() < 1e-12);
}

#[tokio::test]
async fn streamed_tool_arguments_arrive_as_deltas_and_parse_at_the_end() {
    let provider = MockProvider::sse(ENDPOINT, "completions_tool_call.sse").await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::ToolCallStart(0),
            Ev::ToolCallDelta(0, "".into()),
            Ev::ToolCallDelta(0, "{\"cmd\":".into()),
            Ev::ToolCallDelta(0, "\"ls -la".into()),
            Ev::ToolCallDelta(0, "\"}".into()),
            Ev::ToolCallEnd(0, "bash".into(), r#"{"cmd":"ls -la"}"#.into()),
            Ev::Done("toolUse".into()),
        ]
    );

    let call = collected.terminal.tool_calls().next().unwrap();
    assert_eq!(call.id, "call_abc");
    assert_eq!(call.name, "bash");
    assert_eq!(call.arguments["cmd"], json!("ls -la"));
    assert_eq!(collected.terminal.stop_reason, StopReason::ToolUse);
}

#[tokio::test]
async fn partial_tool_arguments_are_visible_mid_stream() {
    let provider = MockProvider::sse(ENDPOINT, "completions_tool_call.sse").await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    // Index 4 is the delta that closed `"cmd":"ls -la` but not the object; the
    // streaming parser must already expose the partial value.
    let partial = collected.partial_at(4);
    let call = partial.tool_calls().next().unwrap();
    assert_eq!(call.arguments["cmd"], json!("ls -la"));
}

#[tokio::test]
async fn parallel_tool_calls_get_distinct_content_indices() {
    let provider = MockProvider::sse(ENDPOINT, "completions_parallel_tools.sse").await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::TextStart(0),
            Ev::TextDelta(0, "working".into()),
            Ev::ToolCallStart(1),
            Ev::ToolCallDelta(1, r#"{"p":"a"}"#.into()),
            Ev::ToolCallStart(2),
            Ev::ToolCallDelta(2, r#"{"p":"b"}"#.into()),
            // The end events are flushed in content order after the stream closes.
            Ev::TextEnd(0, "working".into()),
            Ev::ToolCallEnd(1, "read".into(), r#"{"p":"a"}"#.into()),
            Ev::ToolCallEnd(2, "read".into(), r#"{"p":"b"}"#.into()),
            Ev::Done("toolUse".into()),
        ]
    );

    let calls: Vec<_> = collected.terminal.tool_calls().collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_a");
    assert_eq!(calls[1].id, "call_b");
}

#[tokio::test]
async fn grammar_custom_tool_input_is_re_encoded_as_json_deltas() {
    let provider = MockProvider::sse(ENDPOINT, "completions_custom_tool.sse").await;
    let mut model = completions_model(&provider.base_url());
    model.compat = Some(compat(|c| c.supports_openai_grammar_tools = Some(true)));

    let mut tool = Tool::new(
        "calc",
        "Evaluate",
        json!({
            "type": "object",
            "properties": { "expr": { "type": "string" } },
            "required": ["expr"]
        }),
    );
    tool.constrained_sampling = Some(ConstrainedSampling::Config(
        ConstrainedSamplingConfig::Grammar {
            variants: GrammarVariants {
                openai_lark: Some("start: expr".into()),
                openai_regex: None,
            },
        },
    ));
    let context = Context::default().with_tools(vec![tool]);

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &context, &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    // The raw grammar text is re-encoded so the concatenated deltas form valid
    // JSON for the tool's single string argument.
    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::ToolCallStart(0),
            Ev::ToolCallDelta(0, "".into()),
            Ev::ToolCallDelta(0, r#"{"expr":"1 + "#.into()),
            Ev::ToolCallDelta(0, r#"2\n"#.into()),
            Ev::ToolCallDelta(0, "\"}".into()),
            Ev::ToolCallEnd(0, "calc".into(), r#"{"expr":"1 + 2\n"}"#.into()),
            Ev::Done("toolUse".into()),
        ]
    );

    let joined: String = collected
        .sequence
        .iter()
        .filter_map(|e| match e {
            Ev::ToolCallDelta(_, delta) => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    let parsed: serde_json::Value = serde_json::from_str(&joined).unwrap();
    assert_eq!(parsed, json!({ "expr": "1 + 2\n" }));

    let call = collected.terminal.tool_calls().next().unwrap();
    assert_eq!(call.arguments["expr"], json!("1 + 2\n"));
}

#[tokio::test]
async fn deepseek_reasoning_content_becomes_a_thinking_block() {
    let provider = MockProvider::sse(ENDPOINT, "completions_deepseek_reasoning.sse").await;
    let mut model = model(
        "deepseek-reasoner",
        Api::OpenAiCompletions,
        "deepseek",
        &provider.base_url(),
    );
    model.reasoning = true;

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::ThinkingStart(0),
            Ev::ThinkingDelta(0, "Let me ".into()),
            Ev::ThinkingDelta(0, "think.".into()),
            Ev::TextStart(1),
            Ev::TextDelta(1, "42".into()),
            Ev::ThinkingEnd(0, "Let me think.".into()),
            Ev::TextEnd(1, "42".into()),
            Ev::Done("stop".into()),
        ]
    );

    let thinking = collected.terminal.content[0].as_thinking().unwrap();
    assert_eq!(thinking.thinking, "Let me think.");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some("reasoning_content")
    );
    // DeepSeek reports cache reads under `prompt_cache_hit_tokens`.
    assert_eq!(collected.terminal.usage.cache_read, 40);
    assert_eq!(collected.terminal.usage.input, 60);
}

#[tokio::test]
async fn missing_finish_reason_is_inferred_when_compat_says_so() {
    let provider = MockProvider::sse(ENDPOINT, "completions_no_finish_reason.sse").await;
    let mut model = model(
        "local-model",
        Api::OpenAiCompletions,
        "custom",
        &provider.base_url(),
    );
    model.compat = Some(compat(|c| c.supports_finish_reason = Some(false)));

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(collected.sequence.last(), Some(&Ev::Done("stop".into())));
    assert_eq!(collected.terminal.stop_reason, StopReason::Stop);
    assert_eq!(collected.terminal.text(), "partial answer");
    assert_eq!(collected.terminal.raw_stop_reason, None);
}

#[tokio::test]
async fn missing_finish_reason_is_an_error_when_the_provider_promises_one() {
    let provider = MockProvider::sse(ENDPOINT, "completions_no_finish_reason.sse").await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence.last(),
        Some(&Ev::Error(
            "error".into(),
            "protocol error: Stream ended without finish_reason".into()
        ))
    );
    assert_eq!(collected.terminal.stop_reason, StopReason::Error);
    // The partial content survives on the error message.
    assert_eq!(collected.terminal.text(), "partial answer");
}

#[tokio::test]
async fn http_errors_are_encoded_in_the_stream_not_returned() {
    let provider = MockProvider::raw(
        ENDPOINT,
        r#"{"error":{"message":"model not found","type":"invalid_request_error"}}"#,
        404,
    )
    .await;
    let model = completions_model(&provider.base_url());

    // `stream` must resolve Ok even though the request fails.
    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .expect("stream() must not return Err for a provider failure");
    let collected = collect(stream).await;

    assert_eq!(collected.sequence.len(), 1);
    let Ev::Error(reason, message) = &collected.sequence[0] else {
        panic!("expected an error event, got {:?}", collected.sequence[0]);
    };
    assert_eq!(reason, "error");
    assert!(message.contains("model not found"), "got: {message}");
    assert_eq!(collected.terminal.stop_reason, StopReason::Error);
}

#[tokio::test]
async fn aborting_produces_an_aborted_error_event() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let model = completions_model(&provider.base_url());

    let (handle, signal) = pi_core::options::AbortHandle::new();
    handle.abort();
    let mut options = options_with_key("sk-test");
    options.request.signal = Some(signal);

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert!(matches!(collected.sequence.last(), Some(Ev::Error(r, _)) if r == "aborted"));
    assert_eq!(collected.terminal.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn a_content_filter_finish_reason_terminates_with_an_error() {
    let body = concat!(
        "data: {\"id\":\"c1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let provider = MockProvider::raw(ENDPOINT, body, 200).await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence.last(),
        Some(&Ev::Error(
            "error".into(),
            "Provider finish_reason: content_filter".into()
        ))
    );
    assert_eq!(
        collected.terminal.raw_stop_reason.as_deref(),
        Some("content_filter")
    );
}

#[tokio::test]
async fn response_model_is_recorded_when_it_differs() {
    let body = concat!(
        "data: {\"id\":\"c1\",\"model\":\"gpt-4o-mini-2024-07-18\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let provider = MockProvider::raw(ENDPOINT, body, 200).await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let terminal = collect(stream).await.terminal;
    assert_eq!(
        terminal.response_model.as_deref(),
        Some("gpt-4o-mini-2024-07-18")
    );
}

// ============================================================================
// Request building
// ============================================================================

#[tokio::test]
async fn request_carries_auth_and_streaming_defaults() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let model = completions_model(&provider.base_url());

    let stream = OpenAiCompletionsClient::new()
        .stream(
            &model,
            &Context::new(vec![Message::User(pi_core::UserMessage::text("hi"))]),
            &options_with_key("sk-test"),
        )
        .await
        .unwrap();
    let _ = collect(stream).await;

    let body = provider.request_body();
    assert_eq!(body["model"], "gpt-4o-mini");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hi");

    assert_eq!(
        provider.header("authorization").as_deref(),
        Some("Bearer sk-test")
    );
    assert_eq!(
        provider.header("accept").as_deref(),
        Some("text/event-stream")
    );
}

#[test]
fn system_prompt_uses_developer_role_only_for_reasoning_models_that_support_it() {
    let mut model = completions_model("https://api.openai.com/v1");
    let context = Context::default().with_system_prompt("be brief");

    // Non-reasoning model → system.
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");

    // Reasoning model on a standard endpoint → developer.
    model.reasoning = true;
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["messages"][0]["role"], "developer");

    // Reasoning model on a provider that rejects the developer role → system.
    model.compat = Some(compat(|c| c.supports_developer_role = Some(false)));
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
}

#[test]
fn max_tokens_field_follows_compat() {
    let mut options = options_with_key("k");
    options.max_tokens = Some(2048);

    let openai = completions_model("https://api.openai.com/v1");
    let body = build_body_for(&openai, &Context::default(), &options).unwrap();
    assert_eq!(body["max_completion_tokens"], 2048);
    assert!(body.get("max_tokens").is_none());

    // DeepSeek is detected as a `max_tokens` provider.
    let deepseek = model(
        "deepseek-chat",
        Api::OpenAiCompletions,
        "deepseek",
        "https://api.deepseek.com",
    );
    let body = build_body_for(&deepseek, &Context::default(), &options).unwrap();
    assert_eq!(body["max_tokens"], 2048);
    assert!(body.get("max_completion_tokens").is_none());

    // Explicit compat wins over detection.
    let mut forced = deepseek.clone();
    forced.compat = Some(compat(|c| {
        c.max_tokens_field = Some(MaxTokensField::MaxCompletionTokens)
    }));
    let body = build_body_for(&forced, &Context::default(), &options).unwrap();
    assert_eq!(body["max_completion_tokens"], 2048);
}

#[test]
fn sampling_params_override_named_request_fields() {
    let model = completions_model("https://api.openai.com/v1");
    let mut options = options_with_key("k");
    options.temperature = Some(0.2);
    options.max_tokens = Some(100);
    let mut sampling = serde_json::Map::new();
    sampling.insert("temperature".into(), json!(0.9));
    sampling.insert("top_p".into(), json!(0.1));
    options.sampling_params = Some(sampling);

    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["temperature"], 0.9);
    assert_eq!(body["top_p"], 0.1);
    assert_eq!(body["max_completion_tokens"], 100);
}

// ---------------------------------------------------------------------------
// Thinking formats
// ---------------------------------------------------------------------------

fn reasoning_model(
    provider: &str,
    base_url: &str,
    format: Option<ThinkingFormat>,
) -> pi_core::Model {
    let mut model = model("m", Api::OpenAiCompletions, provider, base_url);
    model.reasoning = true;
    if let Some(format) = format {
        model.compat = Some(compat(|c| c.thinking_format = Some(format)));
    }
    model
}

fn with_effort(effort: &str) -> pi_core::options::StreamOptions {
    pi_provider_openai::with_provider_option(
        options_with_key("k"),
        ProviderOptionKey::ReasoningEffort,
        json!(effort),
    )
}

#[test]
fn openai_thinking_format_sends_reasoning_effort() {
    let model = reasoning_model("openai", "https://api.openai.com/v1", None);
    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn openrouter_thinking_format_nests_the_effort() {
    let model = reasoning_model("openrouter", "https://openrouter.ai/api/v1", None);
    let body = build_body_for(&model, &Context::default(), &with_effort("low")).unwrap();
    assert_eq!(body["reasoning"], json!({ "effort": "low" }));

    // With no effort, OpenRouter is told explicitly that reasoning is off.
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(body["reasoning"], json!({ "effort": "none" }));
}

#[test]
fn deepseek_thinking_format_toggles_a_thinking_object() {
    let model = reasoning_model("deepseek", "https://api.deepseek.com", None);
    let body = build_body_for(&model, &Context::default(), &with_effort("medium")).unwrap();
    assert_eq!(body["thinking"], json!({ "type": "enabled" }));
    // DeepSeek is detected as not supporting reasoning_effort? It does.
    assert_eq!(body["reasoning_effort"], "medium");

    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(body["thinking"], json!({ "type": "disabled" }));
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn together_thinking_format_uses_an_enabled_flag() {
    let model = reasoning_model("together", "https://api.together.ai/v1", None);
    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(body["reasoning"], json!({ "enabled": true }));
    // Together is detected as not supporting reasoning_effort.
    assert!(body.get("reasoning_effort").is_none());

    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(body["reasoning"], json!({ "enabled": false }));
}

#[test]
fn zai_thinking_format_sends_a_clear_thinking_flag() {
    let model = reasoning_model("zai", "https://api.z.ai/api/coding/paas/v4", None);
    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(
        body["thinking"],
        json!({ "type": "enabled", "clear_thinking": false })
    );
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(body["thinking"], json!({ "type": "disabled" }));
}

#[test]
fn qwen_thinking_format_uses_enable_thinking() {
    let model = reasoning_model(
        "qwen-token-plan",
        "https://qwen.example/v1",
        Some(ThinkingFormat::Qwen),
    );
    let body = build_body_for(&model, &Context::default(), &with_effort("low")).unwrap();
    assert_eq!(body["enable_thinking"], true);
    assert_eq!(body["reasoning_effort"], "low");

    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(body["enable_thinking"], false);
}

#[test]
fn qwen_chat_template_format_sets_template_kwargs() {
    let model = reasoning_model(
        "vllm",
        "https://vllm.example/v1",
        Some(ThinkingFormat::QwenChatTemplate),
    );
    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(
        body["chat_template_kwargs"],
        json!({ "enable_thinking": true, "preserve_thinking": true })
    );
}

#[test]
fn string_thinking_format_sends_a_bare_string() {
    let model = reasoning_model(
        "custom",
        "https://custom.example/v1",
        Some(ThinkingFormat::StringThinking),
    );
    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(body["thinking"], "high");
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(body["thinking"], "none");
}

#[test]
fn ant_ling_format_only_sends_mapped_efforts() {
    let mut model = reasoning_model("ant-ling", "https://api.ant-ling.com/v1", None);
    // Without a mapping, ant-ling sends nothing.
    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert!(body.get("reasoning").is_none());

    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::High, Some("deep"))]));
    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(body["reasoning"], json!({ "effort": "deep" }));
}

#[test]
fn chat_template_format_substitutes_var_descriptors() {
    let mut model = reasoning_model(
        "vllm",
        "https://vllm.example/v1",
        Some(ThinkingFormat::ChatTemplate),
    );
    model.thinking_level_map = Some(thinking_map(&[
        (ModelThinkingLevel::Off, Some("off")),
        (ModelThinkingLevel::High, Some("deep")),
    ]));
    let mut kwargs = serde_json::Map::new();
    kwargs.insert("literal".into(), json!("keep-me"));
    kwargs.insert("thinking".into(), json!({ "$var": "thinking.enabled" }));
    kwargs.insert("effort".into(), json!({ "$var": "thinking.level" }));
    kwargs.insert(
        "only_when_on".into(),
        json!({ "$var": "thinking.level", "omitWhenOff": true }),
    );
    model.compat = Some(compat(|c| {
        c.thinking_format = Some(ThinkingFormat::ChatTemplate);
        c.chat_template_kwargs = Some(kwargs);
    }));

    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(
        body["chat_template_kwargs"],
        json!({ "literal": "keep-me", "thinking": true, "effort": "deep", "only_when_on": "deep" })
    );

    // With reasoning off, `$var` resolves through the `off` mapping and
    // `omitWhenOff` keys disappear entirely.
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(
        body["chat_template_kwargs"],
        json!({ "literal": "keep-me", "thinking": false, "effort": "off" })
    );
}

#[test]
fn baseten_format_uses_chat_template_args() {
    let mut model = reasoning_model("baseten", "https://inference.baseten.co/v1", None);
    let mut args = serde_json::Map::new();
    args.insert("reasoning".into(), json!({ "$var": "thinking.enabled" }));
    model.compat = Some(compat(|c| {
        c.thinking_format = Some(ThinkingFormat::Baseten);
        c.chat_template_args = Some(args);
    }));
    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::High, Some("high"))]));

    let body = build_body_for(&model, &Context::default(), &with_effort("high")).unwrap();
    assert_eq!(body["chat_template_args"], json!({ "reasoning": true }));
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn thinking_token_budget_reserves_room_for_the_answer() {
    let mut model = reasoning_model(
        "vllm",
        "https://vllm.example/v1",
        Some(ThinkingFormat::Qwen),
    );
    model.compat = Some(compat(|c| {
        c.thinking_format = Some(ThinkingFormat::Qwen);
        c.supports_thinking_token_budget = Some(true);
    }));
    let mut options = with_effort("high");
    options.max_tokens = Some(4096);

    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    // The `high` default budget is 16384, clamped to 4096 - 1024.
    assert_eq!(body["thinking_token_budget"], 3072);

    // A ceiling at or below the answer reserve emits no budget at all.
    options.max_tokens = Some(1024);
    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert!(body.get("thinking_token_budget").is_none());
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn strict_tool() -> Tool {
    let mut tool = Tool::new(
        "search",
        "Search files",
        json!({
            "type": "object",
            "properties": { "query": { "type": "string" }, "limit": { "type": "number" } },
            "required": ["query"]
        }),
    );
    tool.constrained_sampling = Some(ConstrainedSampling::Config(
        ConstrainedSamplingConfig::JsonSchema {
            strict: StrictMode::Prefer,
        },
    ));
    tool
}

#[test]
fn strict_tools_rewrite_the_schema_and_set_the_flag() {
    let model = completions_model("https://api.openai.com/v1");
    let context = Context::default().with_tools(vec![strict_tool()]);
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();

    let tool = &body["tools"][0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["function"]["strict"], true);
    assert_eq!(
        tool["function"]["parameters"]["required"],
        json!(["query", "limit"])
    );
    assert_eq!(
        tool["function"]["parameters"]["additionalProperties"],
        false
    );
    assert_eq!(
        tool["function"]["parameters"]["properties"]["limit"],
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
    );
}

#[test]
fn providers_without_strict_mode_omit_the_flag_entirely() {
    let model = model(
        "m",
        Api::OpenAiCompletions,
        "together",
        "https://api.together.ai/v1",
    );
    let context = Context::default().with_tools(vec![strict_tool()]);
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();

    let function = &body["tools"][0]["function"];
    assert!(function.get("strict").is_none());
    // The schema is left untouched when strict is not applied.
    assert_eq!(function["parameters"]["required"], json!(["query"]));
}

#[test]
fn grammar_tools_become_openai_custom_tools() {
    let mut tool = Tool::new(
        "calc",
        "Evaluate an expression",
        json!({
            "type": "object",
            "properties": { "expr": { "type": "string" } },
            "required": ["expr"]
        }),
    );
    tool.constrained_sampling = Some(ConstrainedSampling::Config(
        ConstrainedSamplingConfig::Grammar {
            variants: GrammarVariants {
                openai_lark: Some("start: SIGNED_NUMBER".into()),
                openai_regex: None,
            },
        },
    ));

    let mut model = completions_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| c.supports_openai_grammar_tools = Some(true)));
    let context = Context::default().with_tools(vec![tool.clone()]);
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();

    assert_eq!(
        body["tools"][0],
        json!({
            "type": "custom",
            "custom": {
                "name": "calc",
                "description": "Evaluate an expression",
                "format": {
                    "type": "grammar",
                    "grammar": { "syntax": "lark", "definition": "start: SIGNED_NUMBER" }
                }
            }
        })
    );

    // Without the compat flag the same tool falls back to a function tool.
    let plain = completions_model("https://api.openai.com/v1");
    let body = build_body_for(&plain, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["tools"][0]["type"], "function");
}

#[test]
fn a_regex_grammar_tool_uses_the_regex_syntax() {
    let mut tool = Tool::new(
        "zip",
        "A US zip code",
        json!({
            "type": "object",
            "properties": { "code": { "type": "string" } },
            "required": ["code"]
        }),
    );
    tool.constrained_sampling = Some(ConstrainedSampling::Config(
        ConstrainedSamplingConfig::Grammar {
            variants: GrammarVariants {
                openai_lark: None,
                openai_regex: Some(r"\d{5}".into()),
            },
        },
    ));
    let mut model = completions_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| c.supports_openai_grammar_tools = Some(true)));
    let context = Context::default().with_tools(vec![tool]);
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(
        body["tools"][0]["custom"]["format"]["grammar"]["syntax"],
        "regex"
    );
}

#[test]
fn an_empty_tools_array_is_sent_when_the_transcript_has_tool_history() {
    let model = completions_model("https://api.openai.com/v1");
    let mut assistant =
        pi_core::AssistantMessage::pending("openai-completions", "openai", "gpt-4o-mini");
    assistant.content = vec![AssistantContent::ToolCall(pi_core::ToolCall::new(
        "c1", "bash",
    ))];
    assistant.stop_reason = StopReason::ToolUse;
    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(pi_core::ToolResultMessage::text("c1", "bash", "ok", false)),
    ]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["tools"], json!([]));
}

#[test]
fn tool_choice_passes_through() {
    let model = completions_model("https://api.openai.com/v1");
    let options = pi_provider_openai::with_provider_option(
        options_with_key("k"),
        ProviderOptionKey::ToolChoice,
        json!({ "type": "function", "function": { "name": "bash" } }),
    );
    let context = Context::default().with_tools(vec![Tool::no_params("bash", "run")]);
    let body = build_body_for(&model, &context, &options).unwrap();
    assert_eq!(body["tool_choice"]["function"]["name"], "bash");
}

// ---------------------------------------------------------------------------
// Prompt cache, session affinity and routing
// ---------------------------------------------------------------------------

#[test]
fn prompt_cache_key_is_sent_for_openai_and_clamped_to_64_chars() {
    let model = completions_model("https://api.openai.com/v1");
    let mut options = options_with_key("k");
    options.session_id = Some("s".repeat(100));
    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["prompt_cache_key"].as_str().unwrap().len(), 64);

    // A third-party endpoint gets no cache key on short retention.
    let other = model_at("https://api.groq.com/openai/v1", "groq");
    let body = build_body_for(&other, &Context::default(), &options).unwrap();
    assert!(body.get("prompt_cache_key").is_none());
}

fn model_at(base_url: &str, provider: &str) -> pi_core::Model {
    model("m", Api::OpenAiCompletions, provider, base_url)
}

#[test]
fn long_retention_adds_the_24h_flag_where_supported() {
    let model = model_at("https://api.groq.com/openai/v1", "groq");
    let mut options = options_with_key("k");
    options.session_id = Some("session-1".into());
    options.cache_retention = Some(CacheRetention::Long);

    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["prompt_cache_retention"], "24h");
    assert_eq!(body["prompt_cache_key"], "session-1");

    // Together does not support long retention: neither key nor flag.
    let together = model_at("https://api.together.ai/v1", "together");
    let body = build_body_for(&together, &Context::default(), &options).unwrap();
    assert!(body.get("prompt_cache_retention").is_none());
    assert!(body.get("prompt_cache_key").is_none());
}

#[test]
fn cache_retention_none_suppresses_the_cache_key() {
    let model = completions_model("https://api.openai.com/v1");
    let mut options = options_with_key("k");
    options.session_id = Some("session-1".into());
    options.cache_retention = Some(CacheRetention::None);
    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert!(body.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn session_affinity_headers_use_the_openai_format() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let mut model = completions_model(&provider.base_url());
    model.compat = Some(compat(|c| {
        c.send_session_affinity_headers = Some(true);
        c.session_affinity_format = Some(SessionAffinityFormat::Openai);
    }));
    let mut options = options_with_key("k");
    options.session_id = Some("sess-42".into());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    assert_eq!(provider.header("session_id").as_deref(), Some("sess-42"));
    assert_eq!(
        provider.header("x-client-request-id").as_deref(),
        Some("sess-42")
    );
    assert_eq!(
        provider.header("x-session-affinity").as_deref(),
        Some("sess-42")
    );
    assert!(provider.header("x-session-id").is_none());
}

#[tokio::test]
async fn session_affinity_headers_use_the_openrouter_format() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let mut model = model(
        "m",
        Api::OpenAiCompletions,
        "openrouter",
        &provider.base_url(),
    );
    model.compat = Some(compat(|c| c.send_session_affinity_headers = Some(true)));
    let mut options = options_with_key("k");
    options.session_id = Some("sess-42".into());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    assert_eq!(provider.header("x-session-id").as_deref(), Some("sess-42"));
    assert!(provider.header("session_id").is_none());
    assert!(provider.header("x-session-affinity").is_none());
}

#[tokio::test]
async fn session_affinity_headers_use_the_openai_nosession_format() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let mut model = completions_model(&provider.base_url());
    model.compat = Some(compat(|c| {
        c.send_session_affinity_headers = Some(true);
        c.session_affinity_format = Some(SessionAffinityFormat::OpenaiNosession);
    }));
    let mut options = options_with_key("k");
    options.session_id = Some("sess-42".into());

    let stream = OpenAiCompletionsClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    // The "nosession" variant omits `session_id` but keeps the other two.
    assert!(provider.header("session_id").is_none());
    assert_eq!(
        provider.header("x-client-request-id").as_deref(),
        Some("sess-42")
    );
    assert_eq!(
        provider.header("x-session-affinity").as_deref(),
        Some("sess-42")
    );
}

#[test]
fn openrouter_and_vercel_routing_land_in_the_payload() {
    let mut model = model_at("https://openrouter.ai/api/v1", "openrouter");
    model.compat = Some(compat(|c| {
        c.open_router_routing = Some(json!({ "order": ["anthropic"], "allow_fallbacks": false }));
    }));
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(body["provider"]["order"], json!(["anthropic"]));

    let mut model = model_at("https://ai-gateway.vercel.sh", "vercel-ai-gateway");
    model.compat = Some(compat(|c| {
        c.vercel_gateway_routing =
            Some(json!({ "only": ["bedrock"], "order": ["bedrock", "anthropic"] }));
    }));
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert_eq!(
        body["providerOptions"]["gateway"],
        json!({ "only": ["bedrock"], "order": ["bedrock", "anthropic"] })
    );
}

#[test]
fn anthropic_cache_control_marks_system_last_tool_and_last_message() {
    let mut model = model_at("https://openrouter.ai/api/v1", "openrouter");
    model.id = "anthropic/claude-sonnet".into();
    let context = Context::new(vec![Message::User(pi_core::UserMessage::text("hello"))])
        .with_system_prompt("sys")
        .with_tools(vec![Tool::no_params("a", "x"), Tool::no_params("b", "y")]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert_eq!(
        body["messages"][1]["content"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(
        body["tools"][1]["cache_control"],
        json!({ "type": "ephemeral" })
    );
}

// ---------------------------------------------------------------------------
// Message transforms
// ---------------------------------------------------------------------------

#[test]
fn tool_results_are_grouped_and_can_require_a_name() {
    let mut model = completions_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| c.requires_tool_result_name = Some(true)));

    let mut assistant =
        pi_core::AssistantMessage::pending("openai-completions", "openai", "gpt-4o-mini");
    assistant.content = vec![AssistantContent::ToolCall(pi_core::ToolCall::new(
        "c1", "bash",
    ))];
    assistant.stop_reason = StopReason::ToolUse;
    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(pi_core::ToolResultMessage::text(
            "c1", "bash", "output", false,
        )),
    ]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    let tool_message = &body["messages"][1];
    assert_eq!(tool_message["role"], "tool");
    assert_eq!(tool_message["tool_call_id"], "c1");
    assert_eq!(tool_message["name"], "bash");
    assert_eq!(tool_message["content"], "output");
}

#[test]
fn empty_tool_results_get_a_placeholder() {
    let model = completions_model("https://api.openai.com/v1");
    let mut result = pi_core::ToolResultMessage::text("c1", "bash", "", false);
    result.content = vec![];
    let context = Context::new(vec![Message::ToolResult(result)]);
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["messages"][0]["content"], "(no tool output)");
}

#[test]
fn thinking_is_replayed_as_text_when_compat_demands_it() {
    let mut model = completions_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| c.requires_thinking_as_text = Some(true)));

    let mut assistant =
        pi_core::AssistantMessage::pending("openai-completions", "openai", "gpt-4o-mini");
    assistant.content = vec![
        AssistantContent::thinking("private thoughts"),
        AssistantContent::text("public answer"),
    ];
    assistant.stop_reason = StopReason::Stop;
    let context = Context::new(vec![Message::Assistant(assistant)]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            { "type": "text", "text": "private thoughts" },
            { "type": "text", "text": "public answer" }
        ])
    );
}

#[test]
fn thinking_signature_becomes_a_sibling_field_by_default() {
    let model = completions_model("https://api.openai.com/v1");
    let mut assistant =
        pi_core::AssistantMessage::pending("openai-completions", "openai", "gpt-4o-mini");
    assistant.content = vec![
        AssistantContent::Thinking(pi_core::ThinkingContent {
            thinking: "hmm".into(),
            thinking_signature: Some("reasoning_content".into()),
            redacted: false,
        }),
        AssistantContent::text("answer"),
    ];
    assistant.stop_reason = StopReason::Stop;
    let context = Context::new(vec![Message::Assistant(assistant)]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["messages"][0]["content"], "answer");
    assert_eq!(body["messages"][0]["reasoning_content"], "hmm");
}

#[test]
fn deepseek_gets_an_empty_reasoning_content_on_assistant_messages() {
    let mut model = model_at("https://api.deepseek.com", "deepseek");
    model.reasoning = true;
    let mut assistant = pi_core::AssistantMessage::pending("openai-completions", "deepseek", "m");
    assistant.content = vec![AssistantContent::text("answer")];
    assistant.stop_reason = StopReason::Stop;
    let context = Context::new(vec![Message::Assistant(assistant)]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["messages"][0]["reasoning_content"], "");
}

#[test]
fn a_synthetic_assistant_turn_bridges_tool_results_and_user_messages() {
    let mut model = completions_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| {
        c.requires_assistant_after_tool_result = Some(true)
    }));

    let mut assistant =
        pi_core::AssistantMessage::pending("openai-completions", "openai", "gpt-4o-mini");
    assistant.content = vec![AssistantContent::ToolCall(pi_core::ToolCall::new(
        "c1", "bash",
    ))];
    assistant.stop_reason = StopReason::ToolUse;
    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(pi_core::ToolResultMessage::text("c1", "bash", "ok", false)),
        Message::User(pi_core::UserMessage::text("next")),
    ]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, vec!["assistant", "tool", "assistant", "user"]);
    assert_eq!(
        body["messages"][2]["content"],
        "I have processed the tool results."
    );
}

#[test]
fn responses_style_tool_call_ids_are_normalized_for_chat_completions() {
    let model = completions_model("https://api.openai.com/v1");
    // A foreign (different provider) assistant message triggers normalization.
    let mut assistant =
        pi_core::AssistantMessage::pending("openai-responses", "openai-codex", "gpt-5");
    assistant.content = vec![AssistantContent::ToolCall(pi_core::ToolCall::new(
        "call_abc|fc_def",
        "bash",
    ))];
    assistant.stop_reason = StopReason::ToolUse;
    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(pi_core::ToolResultMessage::text(
            "call_abc|fc_def",
            "bash",
            "ok",
            false,
        )),
    ]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["id"],
        "call_abc_fc_def"
    );
    // The tool result id is rewritten to match.
    assert_eq!(body["messages"][1]["tool_call_id"], "call_abc_fc_def");
}

#[test]
fn images_are_downgraded_for_text_only_models() {
    let model = completions_model("https://api.openai.com/v1");
    let mut user = pi_core::UserMessage::text("");
    user.content = pi_core::UserContent::Blocks(vec![
        pi_core::InputContent::text("look:"),
        pi_core::InputContent::image("AAAA", "image/png"),
    ]);
    let context = Context::new(vec![Message::User(user)]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            { "type": "text", "text": "look:" },
            { "type": "text", "text": "(image omitted: model does not support images)" }
        ])
    );
}

#[test]
fn images_survive_for_vision_models() {
    let mut model = completions_model("https://api.openai.com/v1");
    model.input = vec![pi_core::Modality::Text, pi_core::Modality::Image];
    let mut user = pi_core::UserMessage::text("");
    user.content =
        pi_core::UserContent::Blocks(vec![pi_core::InputContent::image("AAAA", "image/png")]);
    let context = Context::new(vec![Message::User(user)]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(
        body["messages"][0]["content"][0]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
}

// ---------------------------------------------------------------------------
// stream_simple
// ---------------------------------------------------------------------------

/// A missing credential is an in-stream `Error` event, the same as it is from
/// `stream`. Upstream's `streamSimple` throws synchronously instead; this port
/// deliberately does not, so an FFI caller has one failure path per condition.
#[tokio::test]
async fn stream_simple_reports_a_missing_credential_in_the_stream() {
    let model = completions_model("https://api.openai.com/v1");
    let stream = OpenAiCompletionsClient::new()
        .stream_simple(
            &model,
            &Context::default(),
            &pi_core::options::SimpleStreamOptions::default(),
        )
        .await
        .expect("stream_simple starts even without a credential");
    let collected = collect(stream).await;
    let Ev::Error(_, message) = collected.sequence.last().unwrap() else {
        panic!(
            "expected an error event, got {:?}",
            collected.sequence.last()
        );
    };
    assert!(
        message.contains("No API key for provider"),
        "got: {message}"
    );
    assert_eq!(collected.terminal.stop_reason, StopReason::Error);
}

#[tokio::test]
async fn stream_simple_maps_reasoning_onto_the_provider_knob() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let mut model = completions_model(&provider.base_url());
    model.reasoning = true;
    model.context_window = 200_000;

    let mut options = pi_core::options::SimpleStreamOptions {
        reasoning: Some(pi_core::model::ThinkingLevel::High),
        ..Default::default()
    };
    options.stream.request.api_key = Some("sk-test".into());

    let stream = OpenAiCompletionsClient::new()
        .stream_simple(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    let body = provider.request_body();
    assert_eq!(body["reasoning_effort"], "high");
    // buildBaseOptions clamps max_tokens into the remaining context window.
    assert!(body["max_completion_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn stream_simple_clamps_an_unsupported_reasoning_level() {
    let provider = MockProvider::sse(ENDPOINT, "completions_text.sse").await;
    let mut model = completions_model(&provider.base_url());
    model.reasoning = true;
    // `xhigh` is only available when explicitly mapped; it clamps down to high.
    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::Minimal, None)]));

    let mut options = pi_core::options::SimpleStreamOptions {
        reasoning: Some(pi_core::model::ThinkingLevel::Xhigh),
        ..Default::default()
    };
    options.stream.request.api_key = Some("sk-test".into());

    let stream = OpenAiCompletionsClient::new()
        .stream_simple(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    assert_eq!(provider.request_body()["reasoning_effort"], "high");
}
