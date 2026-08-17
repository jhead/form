//! Request-payload and header construction.
//!
//! Ports the payload-capture assertions from `test/anthropic-*.test.ts`,
//! `test/cache-retention.test.ts` and the Anthropic half of
//! `test/deferred-tools.test.ts`. Upstream captures the payload by throwing from
//! `onPayload`; here the mock server records the real request instead.

mod common;

use common::*;
use pretty_assertions::assert_eq;
use serde_json::{json, Value};

use pi_core::content::{AssistantContent, InputContent, TextContent, ThinkingContent, ToolCall};
use pi_core::message::{AssistantMessage, Message, StopReason, ToolResultMessage, UserMessage};
use pi_core::model::{CacheRetention, Model, ModelCompat, ModelThinkingLevel, ThinkingLevelMap};
use pi_core::options::SimpleStreamOptions;
use pi_core::tool::{ConstrainedSampling, ConstrainedSamplingConfig, Context, StrictMode, Tool};
use pi_core::{ThinkingLevel, UserContent};

use pi_provider_anthropic::{
    AnthropicEffort, AnthropicOptions, AnthropicToolChoice, ToolChoiceMode,
    FINE_GRAINED_TOOL_STREAMING_BETA, INTERLEAVED_THINKING_BETA,
};

fn lookup_tool() -> Tool {
    Tool::new(
        "lookup",
        "Look up a value",
        json!({ "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] }),
    )
}

fn adaptive_model(compat: ModelCompat) -> Model {
    with_compat(
        model("http://127.0.0.1:1"),
        ModelCompat {
            force_adaptive_thinking: Some(true),
            ..compat
        },
    )
}

fn tools_of(body: &Value) -> Vec<&Value> {
    body["tools"]
        .as_array()
        .map(|t| t.iter().collect())
        .unwrap_or_default()
}

fn tool_names(body: &Value) -> Vec<&str> {
    tools_of(body)
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect()
}

fn last_message_content(body: &Value) -> &Value {
    let messages = body["messages"].as_array().expect("messages");
    &messages.last().expect("a message")["content"]
}

// --- system prompt, cache control ------------------------------------------

#[tokio::test]
async fn caches_the_system_prompt_and_the_trailing_user_block() {
    let context = user_context("Hello").with_system_prompt("You are a helpful assistant.");
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;

    assert_eq!(
        body["system"],
        json!([{
            "type": "text",
            "text": "You are a helpful assistant.",
            "cache_control": { "type": "ephemeral" }
        }])
    );
    // A plain string user message is promoted to a block so it can be marked.
    assert_eq!(
        last_message_content(&body),
        &json!([{ "type": "text", "text": "Hello", "cache_control": { "type": "ephemeral" } }])
    );
}

#[tokio::test]
async fn long_retention_sets_a_one_hour_ttl() {
    let context = user_context("Hello").with_system_prompt("sys");
    let mut options = options_with_key();
    options.cache_retention = Some(CacheRetention::Long);
    let (body, _) = capture_request(&model("x"), &context, &options).await;
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({ "type": "ephemeral", "ttl": "1h" })
    );
}

#[tokio::test]
async fn pi_cache_retention_env_selects_long_retention() {
    let context = user_context("Hello").with_system_prompt("sys");
    let mut options = options_with_key();
    options
        .request
        .env
        .insert("PI_CACHE_RETENTION".into(), "long".into());
    let (body, _) = capture_request(&model("x"), &context, &options).await;
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({ "type": "ephemeral", "ttl": "1h" })
    );
}

#[tokio::test]
async fn long_retention_ttl_is_dropped_when_unsupported() {
    let context = user_context("Hello").with_system_prompt("sys");
    let model = with_compat(
        model("x"),
        ModelCompat {
            supports_long_cache_retention: Some(false),
            ..Default::default()
        },
    );
    let mut options = options_with_key();
    options.cache_retention = Some(CacheRetention::Long);
    let (body, _) = capture_request(&model, &context, &options).await;
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );
}

#[tokio::test]
async fn retention_none_omits_cache_control_everywhere() {
    let context = user_context("Hello")
        .with_system_prompt("sys")
        .with_tools(vec![lookup_tool()]);
    let mut options = options_with_key();
    options.cache_retention = Some(CacheRetention::None);
    let (body, _) = capture_request(&model("x"), &context, &options).await;

    assert!(body["system"][0].get("cache_control").is_none());
    assert!(tools_of(&body)[0].get("cache_control").is_none());
    assert_eq!(last_message_content(&body), &json!("Hello"));
}

#[tokio::test]
async fn caches_the_last_tool_definition_only() {
    let context = user_context("Hello").with_tools(vec![
        lookup_tool(),
        Tool::no_params("second", "Second tool"),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    let tools = tools_of(&body);
    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(tools[1]["cache_control"], json!({ "type": "ephemeral" }));
}

#[tokio::test]
async fn skips_tool_cache_control_when_unsupported() {
    let context = user_context("Hello").with_tools(vec![lookup_tool()]);
    let model = with_compat(
        model("x"),
        ModelCompat {
            supports_cache_control_on_tools: Some(false),
            ..Default::default()
        },
    );
    let (body, _) = capture_request(&model, &context, &options_with_key()).await;
    assert!(tools_of(&body)[0].get("cache_control").is_none());
    // The trailing user block is still cached.
    assert!(last_message_content(&body)[0]
        .get("cache_control")
        .is_some());
}

#[tokio::test]
async fn caches_the_trailing_tool_result_block() {
    let assistant = assistant_with_tool_call();
    let context = Context::new(vec![
        Message::User(UserMessage::text("hi")),
        Message::Assistant(assistant),
        Message::ToolResult(ToolResultMessage::text("call_1", "lookup", "done", false)),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    let content = last_message_content(&body);
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["cache_control"], json!({ "type": "ephemeral" }));
}

// --- tools ------------------------------------------------------------------

#[tokio::test]
async fn sends_per_tool_eager_input_streaming_by_default() {
    let context = user_context("Use the tool").with_tools(vec![lookup_tool()]);
    let (body, headers) = capture_request(
        &adaptive_model(Default::default()),
        &context,
        &options_with_key(),
    )
    .await;

    assert_eq!(tools_of(&body)[0]["eager_input_streaming"], json!(true));
    assert_eq!(header(&headers, "anthropic-beta"), None);
}

#[tokio::test]
async fn falls_back_to_the_legacy_fine_grained_streaming_beta() {
    let context = user_context("Use the tool").with_tools(vec![lookup_tool()]);
    let model = adaptive_model(ModelCompat {
        supports_eager_tool_input_streaming: Some(false),
        ..Default::default()
    });
    let (body, headers) = capture_request(&model, &context, &options_with_key()).await;

    assert!(tools_of(&body)[0].get("eager_input_streaming").is_none());
    assert_eq!(
        header(&headers, "anthropic-beta"),
        Some(FINE_GRAINED_TOOL_STREAMING_BETA)
    );
}

#[tokio::test]
async fn no_fine_grained_beta_without_tools() {
    let model = adaptive_model(ModelCompat {
        supports_eager_tool_input_streaming: Some(false),
        ..Default::default()
    });
    let (body, headers) = capture_request(&model, &user_context("hi"), &options_with_key()).await;

    assert!(body.get("tools").is_none());
    assert_eq!(header(&headers, "anthropic-beta"), None);
}

#[tokio::test]
async fn only_strict_tools_send_the_full_input_schema() {
    let compat_tool = Tool::new(
        "lookup",
        "Look up a value",
        json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"],
            "additionalProperties": false,
            "title": "LookupInput"
        }),
    );
    let model = adaptive_model(ModelCompat {
        supports_strict_tools: Some(true),
        ..Default::default()
    });

    // No constrainedSampling config: legacy shape only.
    let (body, _) = capture_request(
        &model,
        &user_context("hi").with_tools(vec![compat_tool.clone()]),
        &options_with_key(),
    )
    .await;
    assert_eq!(
        tools_of(&body)[0]["input_schema"],
        json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"]
        })
    );
    assert!(tools_of(&body)[0].get("strict").is_none());

    let strict_tool = Tool {
        name: "lookup".into(),
        description: "Look up a value".into(),
        parameters: json!({
            "type": "object",
            "properties": { "value": { "type": "string" }, "optional": { "type": "number" } },
            "required": ["value"],
            "title": "StrictLookupInput"
        }),
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Prefer,
            },
        )),
    };
    let (body, _) = capture_request(
        &model,
        &user_context("hi").with_tools(vec![strict_tool]),
        &options_with_key(),
    )
    .await;
    let tool = tools_of(&body)[0];
    assert_eq!(tool["strict"], json!(true));
    assert_eq!(tool["input_schema"]["additionalProperties"], json!(false));
    assert_eq!(
        tool["input_schema"]["required"],
        json!(["value", "optional"])
    );
    assert_eq!(
        tool["input_schema"]["properties"]["optional"],
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
    );
    assert_eq!(tool["input_schema"]["title"], json!("StrictLookupInput"));
}

#[tokio::test]
async fn strict_required_tools_fail_when_strict_mode_is_unsupported() {
    let tool = Tool {
        name: "lookup".into(),
        description: "Look up a value".into(),
        parameters: json!({ "type": "object", "properties": {}, "required": [] }),
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Require,
            },
        )),
    };
    let events = run_fixture(
        "empty.sse",
        &user_context("hi").with_tools(vec![tool]),
        |_, _| {},
    )
    .await;
    let message = terminal(&events);
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message
        .error_message
        .as_deref()
        .unwrap_or_default()
        .contains("strict tools are unsupported"));
}

#[tokio::test]
async fn forwards_tool_choice_and_metadata() {
    let mut options = options_with_key();
    AnthropicOptions {
        tool_choice: Some(AnthropicToolChoice::tool("lookup")),
        ..Default::default()
    }
    .apply(&mut options);
    options.metadata = Some(
        json!({ "user_id": "user-42", "ignored": 7 })
            .as_object()
            .cloned()
            .unwrap(),
    );

    let context = user_context("hi").with_tools(vec![lookup_tool()]);
    let (body, _) = capture_request(&model("x"), &context, &options).await;
    assert_eq!(
        body["tool_choice"],
        json!({ "type": "tool", "name": "lookup" })
    );
    assert_eq!(body["metadata"], json!({ "user_id": "user-42" }));

    let mut options = options_with_key();
    AnthropicOptions {
        tool_choice: Some(AnthropicToolChoice::Mode(ToolChoiceMode::Any)),
        ..Default::default()
    }
    .apply(&mut options);
    let (body, _) = capture_request(&model("x"), &context, &options).await;
    assert_eq!(body["tool_choice"], json!({ "type": "any" }));
}

// --- thinking ---------------------------------------------------------------

#[tokio::test]
async fn adaptive_thinking_sends_effort_in_output_config() {
    let mut options = options_with_key();
    AnthropicOptions {
        thinking_enabled: Some(true),
        effort: Some(AnthropicEffort::Xhigh),
        ..Default::default()
    }
    .apply(&mut options);

    let (body, headers) = capture_request(
        &adaptive_model(Default::default()),
        &user_context("hi"),
        &options,
    )
    .await;
    assert_eq!(
        body["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(body["output_config"], json!({ "effort": "xhigh" }));
    // Adaptive models have interleaved thinking built in.
    assert_eq!(header(&headers, "anthropic-beta"), None);
}

#[tokio::test]
async fn budget_thinking_is_used_without_force_adaptive_thinking() {
    let mut options = options_with_key();
    AnthropicOptions {
        thinking_enabled: Some(true),
        thinking_budget_tokens: Some(4096),
        ..Default::default()
    }
    .apply(&mut options);

    let (body, headers) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(
        body["thinking"],
        json!({ "type": "enabled", "budget_tokens": 4096, "display": "summarized" })
    );
    assert!(body.get("output_config").is_none());
    assert_eq!(
        header(&headers, "anthropic-beta"),
        Some(INTERLEAVED_THINKING_BETA)
    );
}

#[tokio::test]
async fn interleaved_thinking_beta_can_be_switched_off() {
    let mut options = options_with_key();
    AnthropicOptions {
        interleaved_thinking: Some(false),
        ..Default::default()
    }
    .apply(&mut options);
    let (_, headers) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(header(&headers, "anthropic-beta"), None);
}

#[tokio::test]
async fn thinking_display_can_be_omitted() {
    let mut options = options_with_key();
    AnthropicOptions {
        thinking_enabled: Some(true),
        thinking_display: Some(pi_provider_anthropic::AnthropicThinkingDisplay::Omitted),
        ..Default::default()
    }
    .apply(&mut options);
    let (body, _) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(body["thinking"]["display"], json!("omitted"));
}

#[tokio::test]
async fn thinking_disabled_is_explicit_when_reasoning_is_off() {
    let (body, _) = capture_simple_request(
        &adaptive_model(Default::default()),
        &user_context("hi"),
        &simple_options_with_key(),
    )
    .await;
    assert_eq!(body["thinking"], json!({ "type": "disabled" }));
    assert!(body.get("output_config").is_none());
}

#[tokio::test]
async fn thinking_is_omitted_for_models_that_reject_disabling_it() {
    let mut model = adaptive_model(Default::default());
    let mut map = ThinkingLevelMap::new();
    // `off: null` upstream = the model cannot turn thinking off.
    map.insert(ModelThinkingLevel::Off, None);
    model.thinking_level_map = Some(map);

    let (body, _) =
        capture_simple_request(&model, &user_context("hi"), &simple_options_with_key()).await;
    assert!(body.get("thinking").is_none());
}

#[tokio::test]
async fn non_reasoning_models_never_send_thinking() {
    let mut model = model("x");
    model.reasoning = false;
    let mut options = options_with_key();
    AnthropicOptions {
        thinking_enabled: Some(true),
        ..Default::default()
    }
    .apply(&mut options);
    let (body, _) = capture_request(&model, &user_context("hi"), &options).await;
    assert!(body.get("thinking").is_none());
}

// --- temperature ------------------------------------------------------------

#[tokio::test]
async fn temperature_respects_compat_and_thinking() {
    let mut options = options_with_key();
    options.temperature = Some(0.0);
    let (body, _) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(body["temperature"], json!(0.0));

    let unsupported = with_compat(
        model("x"),
        ModelCompat {
            supports_temperature: Some(false),
            ..Default::default()
        },
    );
    let (body, _) = capture_request(&unsupported, &user_context("hi"), &options).await;
    assert!(body.get("temperature").is_none());

    // Temperature is incompatible with extended thinking.
    AnthropicOptions {
        thinking_enabled: Some(true),
        ..Default::default()
    }
    .apply(&mut options);
    let (body, _) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert!(body.get("temperature").is_none());
}

// --- max tokens, payload hook, headers --------------------------------------

#[tokio::test]
async fn max_tokens_defaults_to_the_model_cap() {
    let (body, _) = capture_request(&model("x"), &user_context("hi"), &options_with_key()).await;
    assert_eq!(body["max_tokens"], json!(32_000));
    assert_eq!(body["model"], json!("claude-opus-4-8"));
    assert_eq!(body["stream"], json!(true));

    let mut options = options_with_key();
    options.max_tokens = Some(256);
    let (body, _) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(body["max_tokens"], json!(256));
}

#[tokio::test]
async fn on_payload_can_replace_the_body() {
    let mut options = options_with_key();
    options.request.on_payload = Some(std::sync::Arc::new(|payload: &Value, _: &Model| {
        let mut payload = payload.clone();
        payload["max_tokens"] = json!(7);
        Some(payload)
    }));
    let (body, _) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(body["max_tokens"], json!(7));
}

#[tokio::test]
async fn sends_api_key_and_version_headers() {
    let (_, headers) = capture_request(&model("x"), &user_context("hi"), &options_with_key()).await;
    assert_eq!(header(&headers, "x-api-key"), Some("test-key"));
    assert_eq!(header(&headers, "anthropic-version"), Some("2023-06-01"));
    assert_eq!(
        header(&headers, "anthropic-dangerous-direct-browser-access"),
        Some("true")
    );
    assert_eq!(header(&headers, "authorization"), None);
}

#[tokio::test]
async fn caller_headers_override_and_remove_defaults() {
    let mut options = options_with_key();
    options.request.headers.insert("x-api-key".into(), None);
    options
        .request
        .headers
        .insert("X-Custom".into(), Some("1".into()));
    let (_, headers) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(header(&headers, "x-api-key"), None);
    assert_eq!(header(&headers, "x-custom"), Some("1"));
}

#[tokio::test]
async fn sends_session_affinity_only_when_enabled() {
    let mut options = options_with_key();
    options.session_id = Some("session-1".into());
    let (_, headers) = capture_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(header(&headers, "x-session-affinity"), None);

    let model = with_compat(
        model("x"),
        ModelCompat {
            send_session_affinity_headers: Some(true),
            ..Default::default()
        },
    );
    let (_, headers) = capture_request(&model, &user_context("hi"), &options).await;
    assert_eq!(header(&headers, "x-session-affinity"), Some("session-1"));

    // Caching off means no affinity either.
    options.cache_retention = Some(CacheRetention::None);
    let (_, headers) = capture_request(&model, &user_context("hi"), &options).await;
    assert_eq!(header(&headers, "x-session-affinity"), None);
}

// --- OAuth ------------------------------------------------------------------

#[tokio::test]
async fn oauth_requests_carry_claude_code_identity() {
    let mut options = options_with_key();
    options.request.api_key = Some("sk-ant-oat-fake".into());
    let context = user_context("hi")
        .with_system_prompt("Custom system prompt.")
        .with_tools(vec![Tool::no_params("todowrite", "Write todos")]);

    let (body, headers) = capture_request(&model("x"), &context, &options).await;

    assert_eq!(
        body["system"][0]["text"],
        json!("You are Claude Code, Anthropic's official CLI for Claude.")
    );
    assert_eq!(body["system"][1]["text"], json!("Custom system prompt."));
    // Tool names are canonicalized to Claude Code casing on the wire.
    assert_eq!(tool_names(&body), vec!["TodoWrite"]);

    assert_eq!(
        header(&headers, "authorization"),
        Some("Bearer sk-ant-oat-fake")
    );
    assert_eq!(header(&headers, "x-api-key"), None);
    assert_eq!(header(&headers, "x-app"), Some("cli"));
    assert_eq!(header(&headers, "user-agent"), Some("claude-cli/2.1.75"));
    let beta = header(&headers, "anthropic-beta").unwrap_or_default();
    assert!(beta.starts_with("claude-code-20250219,oauth-2025-04-20"));
    assert!(beta.contains(INTERLEAVED_THINKING_BETA));
}

// --- message conversion -----------------------------------------------------

fn assistant_with_tool_call() -> AssistantMessage {
    let mut assistant =
        AssistantMessage::pending("anthropic-messages", "anthropic", "claude-opus-4-8");
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![AssistantContent::ToolCall(ToolCall {
        id: "call_1".into(),
        name: "lookup".into(),
        arguments: json!({ "value": "x" }).as_object().cloned().unwrap(),
        thought_signature: None,
        namespace: None,
    })];
    assistant
}

#[tokio::test]
async fn converts_assistant_thinking_and_tool_calls() {
    let mut assistant = assistant_with_tool_call();
    assistant.content.insert(
        0,
        AssistantContent::Thinking(ThinkingContent {
            thinking: "internal reasoning".into(),
            thinking_signature: Some("sig-1".into()),
            redacted: false,
        }),
    );
    assistant.content.insert(
        1,
        AssistantContent::Thinking(ThinkingContent {
            thinking: "[Reasoning redacted]".into(),
            thinking_signature: Some("opaque".into()),
            redacted: true,
        }),
    );
    let context = Context::new(vec![
        Message::User(UserMessage::text("hi")),
        Message::Assistant(assistant),
        Message::ToolResult(ToolResultMessage::text("call_1", "lookup", "done", false)),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;

    assert_eq!(
        body["messages"][1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "internal reasoning", "signature": "sig-1" },
                { "type": "redacted_thinking", "data": "opaque" },
                { "type": "tool_use", "id": "call_1", "name": "lookup", "input": { "value": "x" } }
            ]
        })
    );
    assert_eq!(
        body["messages"][2]["content"][0]["tool_use_id"],
        json!("call_1")
    );
    assert_eq!(body["messages"][2]["content"][0]["is_error"], json!(false));
}

#[tokio::test]
async fn empty_thinking_signatures_downgrade_to_text_unless_allowed() {
    let mut assistant =
        AssistantMessage::pending("anthropic-messages", "anthropic", "claude-opus-4-8");
    assistant.stop_reason = StopReason::Stop;
    assistant.content = vec![AssistantContent::Thinking(ThinkingContent {
        thinking: "internal reasoning".into(),
        thinking_signature: Some(" ".into()),
        redacted: false,
    })];
    let context = Context::new(vec![
        Message::User(UserMessage::text("first")),
        Message::Assistant(assistant),
        Message::User(UserMessage::text("second")),
    ]);

    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    assert_eq!(
        body["messages"][1]["content"],
        json!([{ "type": "text", "text": "internal reasoning" }])
    );

    let tolerant = with_compat(
        model("x"),
        ModelCompat {
            allow_empty_signature: Some(true),
            ..Default::default()
        },
    );
    let (body, _) = capture_request(&tolerant, &context, &options_with_key()).await;
    assert_eq!(
        body["messages"][1]["content"],
        json!([{ "type": "thinking", "thinking": "internal reasoning", "signature": "" }])
    );
}

#[tokio::test]
async fn keeps_thinking_with_a_signature_even_when_the_text_is_empty() {
    let mut assistant =
        AssistantMessage::pending("anthropic-messages", "anthropic", "claude-opus-4-8");
    assistant.stop_reason = StopReason::Stop;
    assistant.content = vec![AssistantContent::Thinking(ThinkingContent {
        thinking: String::new(),
        thinking_signature: Some("signed-thinking".into()),
        redacted: false,
    })];
    let context = Context::new(vec![
        Message::User(UserMessage::text("first")),
        Message::Assistant(assistant),
        Message::User(UserMessage::text("second")),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    assert_eq!(
        body["messages"][1]["content"],
        json!([{ "type": "thinking", "thinking": "", "signature": "signed-thinking" }])
    );
}

#[tokio::test]
async fn synthesizes_results_for_orphaned_tool_calls() {
    let context = Context::new(vec![
        Message::User(UserMessage::text("hi")),
        Message::Assistant(assistant_with_tool_call()),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;

    let content = last_message_content(&body);
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["tool_use_id"], json!("call_1"));
    assert_eq!(content[0]["content"], json!("No result provided"));
    assert_eq!(content[0]["is_error"], json!(true));
}

#[tokio::test]
async fn drops_errored_assistant_turns() {
    let mut errored =
        AssistantMessage::pending("anthropic-messages", "anthropic", "claude-opus-4-8");
    errored.stop_reason = StopReason::Error;
    errored.content = vec![AssistantContent::text("half a thought")];
    let context = Context::new(vec![
        Message::User(UserMessage::text("hi")),
        Message::Assistant(errored),
        Message::User(UserMessage::text("again")),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["role"].as_str())
        .collect();
    assert_eq!(roles, vec!["user", "user"]);
}

#[tokio::test]
async fn downgrades_images_for_text_only_models() {
    let context = Context::new(vec![Message::User(UserMessage {
        content: UserContent::Blocks(vec![
            InputContent::text("look"),
            InputContent::image("aW1hZ2U=", "image/png"),
        ]),
        timestamp: 1,
    })]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    assert_eq!(
        last_message_content(&body),
        &json!([
            { "type": "text", "text": "look" },
            {
                "type": "text",
                "text": "(image omitted: model does not support images)",
                "cache_control": { "type": "ephemeral" }
            }
        ])
    );

    let mut vision = model("x");
    vision.input = vec![
        pi_core::model::Modality::Text,
        pi_core::model::Modality::Image,
    ];
    let (body, _) = capture_request(&vision, &context, &options_with_key()).await;
    assert_eq!(
        last_message_content(&body)[1]["source"],
        json!({ "type": "base64", "media_type": "image/png", "data": "aW1hZ2U=" })
    );
}

#[tokio::test]
async fn drops_blank_user_messages() {
    let context = Context::new(vec![
        Message::User(UserMessage::text("   ")),
        Message::User(UserMessage::text("real")),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn normalizes_tool_call_ids_from_other_providers() {
    let mut assistant = AssistantMessage::pending("openai-responses", "openai", "gpt-5.4");
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![AssistantContent::ToolCall(ToolCall::new(
        "fc_68d|weird/id",
        "lookup",
    ))];
    let context = Context::new(vec![
        Message::User(UserMessage::text("hi")),
        Message::Assistant(assistant),
        Message::ToolResult(ToolResultMessage::text(
            "fc_68d|weird/id",
            "lookup",
            "done",
            false,
        )),
    ]);
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    assert_eq!(
        body["messages"][1]["content"][0]["id"],
        json!("fc_68d_weird_id")
    );
    assert_eq!(
        body["messages"][2]["content"][0]["tool_use_id"],
        json!("fc_68d_weird_id")
    );
}

// --- deferred tools / tool references ---------------------------------------

fn deferred_context(tools: Vec<Tool>, added: Vec<&str>) -> Context {
    let mut assistant =
        AssistantMessage::pending("anthropic-messages", "anthropic", "claude-opus-4-8");
    assistant.stop_reason = StopReason::ToolUse;
    assistant.content = vec![AssistantContent::ToolCall(ToolCall::new(
        "call_1",
        "base_tool",
    ))];
    let mut result = ToolResultMessage::text("call_1", "base_tool", "done", false);
    result.added_tool_names = Some(added.into_iter().map(str::to_string).collect());
    Context::new(vec![
        Message::User(UserMessage::text("Hello")),
        Message::Assistant(assistant),
        Message::ToolResult(result),
        Message::User(UserMessage::text("Hello")),
    ])
    .with_tools(tools)
}

#[tokio::test]
async fn loads_a_deferred_tool_at_its_tool_result_marker() {
    let context = deferred_context(
        vec![
            Tool::no_params("base_tool", "The base_tool tool"),
            Tool::no_params("late_tool", "The late_tool tool"),
        ],
        vec!["late_tool"],
    );
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;

    assert_eq!(tool_names(&body), vec!["base_tool", "late_tool"]);
    assert!(tools_of(&body)[0].get("defer_loading").is_none());
    assert_eq!(tools_of(&body)[1]["defer_loading"], json!(true));
    assert_eq!(
        body["messages"][2]["content"][0]["content"],
        json!([{ "type": "tool_reference", "tool_name": "late_tool" }])
    );
}

#[tokio::test]
async fn tool_output_moves_to_sibling_blocks_after_a_reference() {
    let mut context = deferred_context(
        vec![
            Tool::no_params("base_tool", "The base_tool tool"),
            Tool::no_params("late_tool", "The late_tool tool"),
        ],
        vec!["late_tool"],
    );
    if let Message::ToolResult(result) = &mut context.messages[2] {
        result.content = vec![
            InputContent::Text(TextContent {
                text: "work completed".into(),
                text_signature: None,
            }),
            InputContent::image("aW1hZ2U=", "image/png"),
        ];
    }
    let mut vision = model("x");
    vision.input = vec![
        pi_core::model::Modality::Text,
        pi_core::model::Modality::Image,
    ];
    let (body, _) = capture_request(&vision, &context, &options_with_key()).await;

    let content = body["messages"][2]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(
        content[0]["content"],
        json!([{ "type": "tool_reference", "tool_name": "late_tool" }])
    );
    assert_eq!(
        content[1],
        json!({ "type": "text", "text": "work completed" })
    );
    assert_eq!(
        content[2]["source"],
        json!({ "type": "base64", "media_type": "image/png", "data": "aW1hZ2U=" })
    );
}

#[tokio::test]
async fn does_not_resurrect_a_marked_tool_missing_from_the_tool_list() {
    let context = deferred_context(
        vec![Tool::no_params("base_tool", "The base_tool tool")],
        vec!["late_tool"],
    );
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    assert_eq!(tool_names(&body), vec!["base_tool"]);
    assert_eq!(body["messages"][2]["content"][0]["content"], json!("done"));
}

#[tokio::test]
async fn keeps_one_immediate_tool_when_every_tool_is_deferred() {
    let context = deferred_context(
        vec![Tool::no_params("late_tool", "The late_tool tool")],
        vec!["late_tool"],
    );
    let (body, _) = capture_request(&model("x"), &context, &options_with_key()).await;
    assert_eq!(tool_names(&body), vec!["late_tool"]);
    assert!(tools_of(&body)[0].get("defer_loading").is_none());
    assert_eq!(body["messages"][2]["content"][0]["content"], json!("done"));
}

#[tokio::test]
async fn tool_references_are_skipped_when_unsupported() {
    let context = deferred_context(
        vec![
            Tool::no_params("base_tool", "The base_tool tool"),
            Tool::no_params("late_tool", "The late_tool tool"),
        ],
        vec!["late_tool"],
    );
    // Haiku rejects client-side tool_reference blocks.
    let mut haiku = model("x");
    haiku.id = "claude-haiku-4-5".into();
    let (body, _) = capture_request(&haiku, &context, &options_with_key()).await;
    assert_eq!(tool_names(&body), vec!["base_tool", "late_tool"]);
    assert!(tools_of(&body)
        .iter()
        .all(|t| t.get("defer_loading").is_none()));

    // ...and an explicit compat override turns them back on.
    let overridden = with_compat(
        haiku,
        ModelCompat {
            supports_tool_references: Some(true),
            ..Default::default()
        },
    );
    let (body, _) = capture_request(&overridden, &context, &options_with_key()).await;
    assert_eq!(tools_of(&body)[1]["defer_loading"], json!(true));
}

#[tokio::test]
async fn oauth_canonicalized_markers_match_active_tools() {
    let context = deferred_context(
        vec![
            Tool::no_params("base_tool", "The base_tool tool"),
            Tool::no_params("read", "Read a file"),
        ],
        vec!["Read"],
    );
    let mut options = options_with_key();
    options.request.api_key = Some("sk-ant-oat-fake".into());
    let (body, _) = capture_request(&model("x"), &context, &options).await;

    assert_eq!(tool_names(&body), vec!["base_tool", "Read"]);
    assert_eq!(tools_of(&body)[1]["defer_loading"], json!(true));
    assert_eq!(
        body["messages"][2]["content"][0]["content"],
        json!([{ "type": "tool_reference", "tool_name": "Read" }])
    );
}

#[tokio::test]
async fn deduplicates_tools_after_oauth_canonicalization() {
    let context = user_context("hi").with_tools(vec![
        Tool::no_params("read", "The read tool"),
        Tool::no_params("Read", "Canonical definition"),
    ]);
    let mut options = options_with_key();
    options.request.api_key = Some("sk-ant-oat-fake".into());
    let (body, _) = capture_request(&model("x"), &context, &options).await;
    assert_eq!(tool_names(&body), vec!["Read"]);
    assert_eq!(
        tools_of(&body)[0]["description"],
        json!("Canonical definition")
    );
}

// --- stream_simple ----------------------------------------------------------

#[tokio::test]
async fn stream_simple_maps_reasoning_to_adaptive_effort() {
    let options = SimpleStreamOptions {
        stream: options_with_key(),
        reasoning: Some(ThinkingLevel::Medium),
        ..Default::default()
    };
    let (body, _) = capture_simple_request(
        &adaptive_model(Default::default()),
        &user_context("hi"),
        &options,
    )
    .await;
    assert_eq!(
        body["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert_eq!(body["output_config"], json!({ "effort": "medium" }));
}

#[tokio::test]
async fn stream_simple_maps_xhigh_through_the_thinking_level_map() {
    let mut model = adaptive_model(Default::default());
    let mut map = ThinkingLevelMap::new();
    map.insert(ModelThinkingLevel::Xhigh, Some("xhigh".into()));
    model.thinking_level_map = Some(map);

    let options = SimpleStreamOptions {
        stream: options_with_key(),
        reasoning: Some(ThinkingLevel::Xhigh),
        ..Default::default()
    };
    let (body, _) = capture_simple_request(&model, &user_context("hi"), &options).await;
    assert_eq!(body["output_config"], json!({ "effort": "xhigh" }));

    // Without a mapping, xhigh clamps to high.
    let (body, _) = capture_simple_request(
        &adaptive_model(Default::default()),
        &user_context("hi"),
        &options,
    )
    .await;
    assert_eq!(body["output_config"], json!({ "effort": "high" }));
}

#[tokio::test]
async fn stream_simple_maps_reasoning_to_a_token_budget() {
    let options = SimpleStreamOptions {
        stream: options_with_key(),
        reasoning: Some(ThinkingLevel::Medium),
        ..Default::default()
    };
    let (body, _) = capture_simple_request(&model("x"), &user_context("hi"), &options).await;
    assert_eq!(
        body["thinking"],
        json!({ "type": "enabled", "budget_tokens": 8192, "display": "summarized" })
    );
    assert_eq!(body["max_tokens"], json!(32_000));
}

#[tokio::test]
async fn stream_simple_honours_custom_thinking_budgets_and_max_tokens() {
    let mut stream = options_with_key();
    stream.max_tokens = Some(2_000);
    let options = SimpleStreamOptions {
        stream,
        reasoning: Some(ThinkingLevel::Low),
        thinking_budgets: Some(pi_core::model::ThinkingBudgets {
            low: Some(1_500),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (body, _) = capture_simple_request(&model("x"), &user_context("hi"), &options).await;
    // maxTokens = min(2000 + 1500, modelMax) = 3500; budget stays under the answer floor.
    assert_eq!(body["max_tokens"], json!(3_500));
    assert_eq!(body["thinking"]["budget_tokens"], json!(1_500));
}

#[tokio::test]
async fn stream_simple_clamps_max_tokens_to_the_context_window() {
    let mut model = model("x");
    model.context_window = 10_000;
    model.max_tokens = 32_000;
    let options = SimpleStreamOptions {
        stream: options_with_key(),
        ..Default::default()
    };
    let (body, _) = capture_simple_request(&model, &user_context("hi"), &options).await;
    // 10_000 - estimate - 4096 safety.
    assert_eq!(body["max_tokens"], json!(5_903));
}

/// The token estimate reaches the wire in UTF-16 code units, as JavaScript's
/// `String.length` does.
///
/// This adapter used to carry its own estimator that counted Unicode scalar
/// values, so a CJK-and-emoji prompt was under-counted and `max_tokens` came
/// out too large. Asserted end to end because the estimator sits three calls
/// away from the request body.
#[tokio::test]
async fn stream_simple_sizes_max_tokens_from_utf16_code_units() {
    let prompt = "日本語🙈🙉🙉🙈café".repeat(100);
    assert_eq!(prompt.chars().count(), 1_100);
    assert_eq!(prompt.encode_utf16().count(), 1_500);

    let mut model = model("x");
    model.context_window = 10_000;
    model.max_tokens = 32_000;
    let options = SimpleStreamOptions {
        stream: options_with_key(),
        ..Default::default()
    };
    let (body, _) = capture_simple_request(&model, &user_context(&prompt), &options).await;
    // 10_000 - ceil(1500 / 4) - 4096 safety = 5_529.
    assert_eq!(body["max_tokens"], json!(5_529));
    // The abandoned scalar count would have left 5_629.
    assert_ne!(body["max_tokens"], json!(5_629));
}
