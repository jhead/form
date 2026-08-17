//! `openai-responses` adapter, end to end against `wiremock`.

mod common;

use common::*;
use pi_core::model::ModelThinkingLevel;
use pi_core::{
    Api, ApiClient, AssistantContent, CacheRetention, Context, Message, StopReason, Tool,
};
use pi_provider_openai::openai_responses::build_body_for;
use pi_provider_openai::options::ProviderOptionKey;
use pi_provider_openai::OpenAiResponsesClient;
use pretty_assertions::assert_eq;
use serde_json::json;

const ENDPOINT: &str = "/responses";

// ============================================================================
// Streaming
// ============================================================================

#[tokio::test]
async fn reasoning_text_and_tool_call_produce_one_interleaved_sequence() {
    let provider = MockProvider::sse(ENDPOINT, "responses_text_and_tool.sse").await;
    let model = responses_model(&provider.base_url());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::ThinkingStart(0),
            Ev::ThinkingDelta(0, "Planning".into()),
            Ev::ThinkingEnd(0, "Planning the answer".into()),
            Ev::TextStart(1),
            Ev::TextDelta(1, "Running".into()),
            Ev::TextDelta(1, " it now".into()),
            Ev::TextEnd(1, "Running it now".into()),
            Ev::ToolCallStart(2),
            Ev::ToolCallDelta(2, "{\"cmd\":\"ls".into()),
            Ev::ToolCallDelta(2, "\"}".into()),
            Ev::ToolCallEnd(2, "bash".into(), r#"{"cmd":"ls"}"#.into()),
            Ev::Done("toolUse".into()),
        ]
    );

    let message = &collected.terminal;
    assert_eq!(message.api, "openai-responses");
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    // `completed` + a tool call is promoted to toolUse.
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("completed"));

    // The reasoning item is persisted verbatim so it can be replayed.
    let thinking = message.content[0].as_thinking().unwrap();
    assert_eq!(thinking.thinking, "Planning the answer");
    let signature: serde_json::Value =
        serde_json::from_str(thinking.thinking_signature.as_deref().unwrap()).unwrap();
    assert_eq!(signature["id"], "rs_1");

    // The text block carries a v1 signature with the message id.
    let text = message.content[1].as_text().unwrap();
    assert_eq!(
        text.text_signature.as_deref(),
        Some(r#"{"v":1,"id":"msg_1"}"#)
    );

    let call = message.tool_calls().next().unwrap();
    assert_eq!(call.id, "call_1|fc_1");
    assert_eq!(call.arguments["cmd"], json!("ls"));
}

#[tokio::test]
async fn responses_usage_subtracts_cached_tokens_from_input() {
    let provider = MockProvider::sse(ENDPOINT, "responses_text_and_tool.sse").await;
    let model = responses_model(&provider.base_url());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let usage = collect(stream).await.terminal.usage;

    assert_eq!(usage.input, 150);
    assert_eq!(usage.cache_read, 50);
    assert_eq!(usage.output, 40);
    assert_eq!(usage.reasoning, Some(18));
    assert_eq!(usage.total_tokens, 240);
}

#[tokio::test]
async fn service_tier_scales_the_cost() {
    let provider = MockProvider::sse(ENDPOINT, "responses_text_and_tool.sse").await;
    let model = responses_model(&provider.base_url());

    let baseline = {
        let stream = OpenAiResponsesClient::new()
            .stream(&model, &Context::default(), &options_with_key("sk-test"))
            .await
            .unwrap();
        collect(stream).await.terminal.usage.cost.total
    };

    let options = pi_provider_openai::with_provider_option(
        options_with_key("sk-test"),
        ProviderOptionKey::ServiceTier,
        json!("flex"),
    );
    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let flex = collect(stream).await.terminal.usage.cost.total;

    assert!((flex - baseline * 0.5).abs() < 1e-12);
}

#[tokio::test]
async fn an_incomplete_response_with_max_output_tokens_is_a_length_stop() {
    let provider = MockProvider::sse(ENDPOINT, "responses_incomplete_length.sse").await;
    let model = responses_model(&provider.base_url());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::TextStart(0),
            Ev::TextDelta(0, "truncated".into()),
            Ev::Done("length".into()),
        ]
    );
    assert_eq!(collected.terminal.stop_reason, StopReason::Length);
    assert_eq!(
        collected.terminal.raw_stop_reason.as_deref(),
        Some("incomplete.max_output_tokens")
    );
}

#[tokio::test]
async fn a_failed_response_becomes_an_error_event() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"boom\"}}}\n\n"
    );
    let provider = MockProvider::raw(ENDPOINT, body, 200).await;
    let model = responses_model(&provider.base_url());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(collected.sequence[0], Ev::Start);
    let Ev::Error(reason, message) = collected.sequence.last().unwrap() else {
        panic!("expected an error event");
    };
    assert_eq!(reason, "error");
    assert!(message.contains("server_error: boom"), "got: {message}");
}

#[tokio::test]
async fn a_stream_that_ends_without_a_terminal_event_is_a_protocol_error() {
    let body =
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\ndata: [DONE]\n\n";
    let provider = MockProvider::raw(ENDPOINT, body, 200).await;
    let model = responses_model(&provider.base_url());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    let Ev::Error(_, message) = collected.sequence.last().unwrap() else {
        panic!("expected an error event");
    };
    assert!(
        message.contains("before a terminal response event"),
        "got: {message}"
    );
}

#[tokio::test]
async fn provider_http_errors_carry_the_azure_style_prefix() {
    let provider =
        MockProvider::raw(ENDPOINT, r#"{"error":{"message":"quota exceeded"}}"#, 429).await;
    let model = responses_model(&provider.base_url());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    let Ev::Error(_, message) = collected.sequence.last().unwrap() else {
        panic!("expected an error event");
    };
    assert!(
        message.starts_with("OpenAI API error (429)"),
        "got: {message}"
    );
}

#[tokio::test]
async fn azure_style_encrypted_reasoning_is_backfilled_from_the_terminal_response() {
    let body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_9\",\"summary\":[]}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_9\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"thought\"}]}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"output\":[{\"type\":\"reasoning\",\"id\":\"rs_9\",\"encrypted_content\":\"ENC\"}]}}\n\n"
    );
    let provider = MockProvider::raw(ENDPOINT, body, 200).await;
    let model = responses_model(&provider.base_url());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("sk-test"))
        .await
        .unwrap();
    let terminal = collect(stream).await.terminal;

    let thinking = terminal.content[0].as_thinking().unwrap();
    let signature: serde_json::Value =
        serde_json::from_str(thinking.thinking_signature.as_deref().unwrap()).unwrap();
    assert_eq!(signature["encrypted_content"], "ENC");
    assert_eq!(signature["id"], "rs_9");
}

// ============================================================================
// Request building
// ============================================================================

#[tokio::test]
async fn request_defaults_match_upstream() {
    let provider = MockProvider::sse(ENDPOINT, "responses_text_and_tool.sse").await;
    let model = responses_model(&provider.base_url());
    let mut options = options_with_key("sk-test");
    options.session_id = Some("sess-1".into());

    let stream = OpenAiResponsesClient::new()
        .stream(
            &model,
            &Context::new(vec![Message::User(pi_core::UserMessage::text("hi"))]),
            &options,
        )
        .await
        .unwrap();
    let _ = collect(stream).await;

    let body = provider.request_body();
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["prompt_cache_key"], "sess-1");
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    // A reasoning model with no requested effort is told reasoning is off.
    assert_eq!(body["reasoning"], json!({ "effort": "none" }));

    assert_eq!(provider.header("session_id").as_deref(), Some("sess-1"));
    assert_eq!(
        provider.header("x-client-request-id").as_deref(),
        Some("sess-1")
    );
}

#[test]
fn max_output_tokens_has_a_floor_of_16() {
    let model = responses_model("https://api.openai.com/v1");
    let mut options = options_with_key("k");
    options.max_tokens = Some(4);
    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["max_output_tokens"], 16);

    options.max_tokens = Some(4096);
    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["max_output_tokens"], 4096);
}

#[test]
fn reasoning_effort_and_summary_request_encrypted_content() {
    let mut model = responses_model("https://api.openai.com/v1");
    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::High, Some("xhigh"))]));

    let options = pi_provider_openai::with_provider_option(
        pi_provider_openai::with_provider_option(
            options_with_key("k"),
            ProviderOptionKey::ReasoningEffort,
            json!("high"),
        ),
        ProviderOptionKey::ReasoningSummary,
        json!("detailed"),
    );
    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(
        body["reasoning"],
        json!({ "effort": "xhigh", "summary": "detailed" })
    );
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn an_explicitly_null_off_level_suppresses_the_reasoning_block() {
    let mut model = responses_model("https://api.openai.com/v1");
    model.thinking_level_map = Some(thinking_map(&[(ModelThinkingLevel::Off, None)]));
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert!(body.get("reasoning").is_none());
}

#[test]
fn github_copilot_never_gets_a_reasoning_off_block() {
    let mut model = responses_model("https://api.individual.githubcopilot.com");
    model.provider = "github-copilot".into();
    let body = build_body_for(&model, &Context::default(), &options_with_key("k")).unwrap();
    assert!(body.get("reasoning").is_none());
}

#[test]
fn explicit_prompt_cache_mode_turns_implicit_caching_off() {
    let mut model = responses_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| {
        c.supports_explicit_prompt_cache_mode = Some(true)
    }));
    let mut options = options_with_key("k");
    options.session_id = Some("s1".into());
    options.cache_retention = Some(CacheRetention::None);

    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["prompt_cache_options"], json!({ "mode": "explicit" }));
    assert!(body.get("prompt_cache_key").is_none());

    // Without the compat flag, `none` merely omits the key.
    let plain = responses_model("https://api.openai.com/v1");
    let body = build_body_for(&plain, &Context::default(), &options).unwrap();
    assert!(body.get("prompt_cache_options").is_none());
}

#[test]
fn long_retention_sets_the_24h_flag() {
    let model = responses_model("https://api.openai.com/v1");
    let mut options = options_with_key("k");
    options.session_id = Some("s1".into());
    options.cache_retention = Some(CacheRetention::Long);
    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["prompt_cache_retention"], "24h");
}

#[test]
fn responses_tools_are_flat_and_default_to_non_strict() {
    let model = responses_model("https://api.openai.com/v1");
    let context = Context::default().with_tools(vec![Tool::new(
        "bash",
        "Run a command",
        json!({ "type": "object", "properties": { "cmd": { "type": "string" } }, "required": ["cmd"] }),
    )]);
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();

    // Responses tools are flat (no `function` wrapper), and `supportsStrictMode`
    // defaults to false, so the key is omitted entirely.
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "bash");
    assert!(body["tools"][0].get("strict").is_none());
}

#[test]
fn deferred_tools_move_out_of_the_prefix_when_additional_tools_is_supported() {
    let mut model = responses_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| c.supports_additional_tools = Some(true)));

    let mut result = pi_core::ToolResultMessage::text("c1", "loader", "loaded", false);
    result.added_tool_names = Some(vec!["late_tool".into()]);
    let mut assistant = pi_core::AssistantMessage::pending("openai-responses", "openai", "gpt-5");
    assistant.content = vec![AssistantContent::ToolCall(pi_core::ToolCall::new(
        "c1", "loader",
    ))];
    assistant.stop_reason = StopReason::ToolUse;

    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(result),
    ])
    .with_tools(vec![
        Tool::no_params("loader", "loads"),
        Tool::no_params("late_tool", "arrives later"),
    ]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();

    // `late_tool` is not in the prefix...
    let prefix_names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(prefix_names, vec!["loader"]);

    // ...it arrives as an `additional_tools` item after the tool result.
    let additional = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "additional_tools")
        .expect("expected an additional_tools item");
    assert_eq!(additional["role"], "developer");
    assert_eq!(additional["tools"][0]["name"], "late_tool");
}

#[test]
fn tool_search_mode_emits_a_search_call_and_output_pair() {
    let mut model = responses_model("https://api.openai.com/v1");
    model.compat = Some(compat(|c| c.supports_tool_search = Some(true)));

    let mut result = pi_core::ToolResultMessage::text("c1", "loader", "loaded", false);
    result.added_tool_names = Some(vec!["late_tool".into()]);
    let mut assistant = pi_core::AssistantMessage::pending("openai-responses", "openai", "gpt-5");
    assistant.content = vec![AssistantContent::ToolCall(pi_core::ToolCall::new(
        "c1", "loader",
    ))];
    assistant.stop_reason = StopReason::ToolUse;

    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(result),
    ])
    .with_tools(vec![
        Tool::no_params("loader", "loads"),
        Tool::no_params("late_tool", "arrives later"),
    ]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    let items = body["input"].as_array().unwrap();
    let search_call = items
        .iter()
        .find(|i| i["type"] == "tool_search_call")
        .expect("tool_search_call");
    let search_output = items
        .iter()
        .find(|i| i["type"] == "tool_search_output")
        .expect("tool_search_output");
    assert_eq!(search_call["call_id"], search_output["call_id"]);
    assert_eq!(search_call["arguments"]["query"], "late_tool");
    assert_eq!(search_output["tools"][0]["defer_loading"], true);
}

#[test]
fn assistant_turns_replay_as_reasoning_message_and_function_call_items() {
    let model = responses_model("https://api.openai.com/v1");
    let mut assistant = pi_core::AssistantMessage::pending("openai-responses", "openai", "gpt-5");
    assistant.content = vec![
        AssistantContent::Thinking(pi_core::ThinkingContent {
            thinking: "thought".into(),
            thinking_signature: Some(r#"{"type":"reasoning","id":"rs_1"}"#.into()),
            redacted: false,
        }),
        AssistantContent::Text(pi_core::TextContent {
            text: "answer".into(),
            text_signature: Some(r#"{"v":1,"id":"msg_1"}"#.into()),
        }),
        AssistantContent::ToolCall(pi_core::ToolCall::new("call_1|fc_1", "bash")),
    ];
    assistant.stop_reason = StopReason::ToolUse;
    let context = Context::new(vec![
        Message::Assistant(assistant),
        Message::ToolResult(pi_core::ToolResultMessage::text(
            "call_1|fc_1",
            "bash",
            "ok",
            false,
        )),
    ]);

    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    let items = body["input"].as_array().unwrap();
    assert_eq!(items[0], json!({ "type": "reasoning", "id": "rs_1" }));
    assert_eq!(items[1]["type"], "message");
    assert_eq!(items[1]["id"], "msg_1");
    assert_eq!(items[1]["content"][0]["text"], "answer");
    assert_eq!(items[2]["type"], "function_call");
    assert_eq!(items[2]["call_id"], "call_1");
    assert_eq!(items[2]["id"], "fc_1");
    assert_eq!(items[3]["type"], "function_call_output");
    assert_eq!(items[3]["call_id"], "call_1");
}

#[test]
fn a_foreign_tool_call_item_id_is_rehashed_to_an_fc_id() {
    let model = responses_model("https://api.openai.com/v1");
    // Same provider+api but a different model → the id is dropped so OpenAI does
    // not run fc/rs pairing validation.
    let mut different_model =
        pi_core::AssistantMessage::pending("openai-responses", "openai", "gpt-4.1");
    different_model.content = vec![AssistantContent::ToolCall(pi_core::ToolCall::new(
        "call_1|fc_1",
        "bash",
    ))];
    different_model.stop_reason = StopReason::ToolUse;

    let context = Context::new(vec![
        Message::Assistant(different_model),
        Message::ToolResult(pi_core::ToolResultMessage::text(
            "call_1|fc_1",
            "bash",
            "ok",
            false,
        )),
    ]);
    let body = build_body_for(&model, &context, &options_with_key("k")).unwrap();
    assert_eq!(body["input"][0]["id"], serde_json::Value::Null);
    assert_eq!(body["input"][0]["call_id"], "call_1");
}

#[test]
fn sampling_params_win_over_named_fields() {
    let model = responses_model("https://api.openai.com/v1");
    let mut options = options_with_key("k");
    options.temperature = Some(0.1);
    let mut sampling = serde_json::Map::new();
    sampling.insert("temperature".into(), json!(1.0));
    sampling.insert("top_logprobs".into(), json!(3));
    options.sampling_params = Some(sampling);

    let body = build_body_for(&model, &Context::default(), &options).unwrap();
    assert_eq!(body["temperature"], 1.0);
    assert_eq!(body["top_logprobs"], 3);
}

#[tokio::test]
async fn openrouter_session_affinity_uses_x_session_id() {
    let provider = MockProvider::sse(ENDPOINT, "responses_text_and_tool.sse").await;
    let model = model(
        "m",
        Api::OpenAiResponses,
        "openrouter",
        &provider.base_url(),
    );
    let mut options = options_with_key("k");
    options.session_id = Some("sess-9".into());

    let stream = OpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    assert_eq!(provider.header("x-session-id").as_deref(), Some("sess-9"));
    assert!(provider.header("session_id").is_none());
}

/// The Responses adapter had the same synchronous credential check in
/// `stream_simple`; like the other three it now reports in-stream instead.
#[tokio::test]
async fn responses_stream_simple_reports_a_missing_credential_in_the_stream() {
    let model = model(
        "m",
        Api::OpenAiResponses,
        "openai",
        "https://api.openai.com/v1",
    );
    let stream = OpenAiResponsesClient::new()
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
