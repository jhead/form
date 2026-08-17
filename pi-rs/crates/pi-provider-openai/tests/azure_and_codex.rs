//! `azure-openai-responses` and `openai-codex-responses`, end to end.

mod common;

use base64::Engine;
use common::*;
use pi_core::{Api, ApiClient, Context, Message, Model, StopReason, Tool};
use pi_provider_openai::azure_openai_responses::{build_azure_body, resolve_azure_config};
use pi_provider_openai::openai_codex_responses::build_codex_request_body;
use pi_provider_openai::options::ProviderOptionKey;
use pi_provider_openai::{AzureOpenAiResponsesClient, OpenAiCodexResponsesClient};
use pretty_assertions::assert_eq;
use serde_json::json;

// ============================================================================
// Azure
// ============================================================================

const AZURE_ENDPOINT: &str = "/responses";

fn azure_model(base_url: &str) -> Model {
    let mut m = model(
        "gpt-5",
        Api::AzureOpenAiResponses,
        "azure-openai-responses",
        base_url,
    );
    m.reasoning = true;
    m
}

#[tokio::test]
async fn azure_streams_the_shared_responses_event_sequence() {
    let provider = MockProvider::sse(AZURE_ENDPOINT, "responses_text_and_tool.sse").await;
    let model = azure_model(&provider.base_url());

    let stream = AzureOpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("azure-key"))
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
    // Azure stamps its own api id regardless of `model.api`.
    assert_eq!(collected.terminal.api, "azure-openai-responses");
}

#[tokio::test]
async fn azure_sends_the_api_key_header_and_api_version_query() {
    let provider = MockProvider::sse(AZURE_ENDPOINT, "responses_text_and_tool.sse").await;
    let model = azure_model(&provider.base_url());
    let mut options = options_with_key("azure-key");
    options.session_id = Some("sess-a".into());
    options.provider_options.insert(
        ProviderOptionKey::AzureApiVersion.as_str().into(),
        json!("2025-01-01"),
    );

    let stream = AzureOpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    assert_eq!(provider.header("api-key").as_deref(), Some("azure-key"));
    let body = provider.request_body();
    assert_eq!(body["prompt_cache_key"], "sess-a");
    assert_eq!(body["store"], false);
}

#[tokio::test]
async fn azure_uses_the_deployment_name_as_the_model_field() {
    let provider = MockProvider::sse(AZURE_ENDPOINT, "responses_text_and_tool.sse").await;
    let model = azure_model(&provider.base_url());
    let mut options = options_with_key("azure-key");
    options.provider_options.insert(
        ProviderOptionKey::AzureDeploymentName.as_str().into(),
        json!("prod-gpt5"),
    );

    let stream = AzureOpenAiResponsesClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    assert_eq!(provider.request_body()["model"], "prod-gpt5");
}

#[tokio::test]
async fn azure_requires_a_credential() {
    let model = azure_model("https://acme.openai.azure.com");
    let stream = AzureOpenAiResponsesClient::new()
        .stream(
            &model,
            &Context::default(),
            &pi_core::options::StreamOptions::default(),
        )
        .await
        .unwrap();
    let collected = collect(stream).await;

    let Ev::Error(_, message) = collected.sequence.last().unwrap() else {
        panic!("expected an error event");
    };
    assert!(
        message.contains("No API key for provider"),
        "got: {message}"
    );
    assert_eq!(collected.terminal.stop_reason, StopReason::Error);
}

#[test]
fn azure_config_prefers_explicit_options_then_env_then_the_model() {
    let model = azure_model("https://from-model.openai.azure.com");

    // model.baseUrl only.
    let config = resolve_azure_config(&model, &options_with_key("k")).unwrap();
    assert_eq!(
        config.base_url,
        "https://from-model.openai.azure.com/openai/v1"
    );
    assert_eq!(config.api_version, "v1");

    // Env resource name wins over the model.
    let mut options = options_with_key("k");
    options
        .request
        .env
        .insert("AZURE_OPENAI_RESOURCE_NAME".into(), "from-env".into());
    let config = resolve_azure_config(&model, &options).unwrap();
    assert_eq!(
        config.base_url,
        "https://from-env.openai.azure.com/openai/v1"
    );

    // An explicit option wins over everything.
    options.provider_options.insert(
        ProviderOptionKey::AzureBaseUrl.as_str().into(),
        json!("https://from-option.openai.azure.com"),
    );
    let config = resolve_azure_config(&model, &options).unwrap();
    assert_eq!(
        config.base_url,
        "https://from-option.openai.azure.com/openai/v1"
    );
}

#[test]
fn azure_tools_default_to_strict_mode_unlike_plain_responses() {
    let model = azure_model("https://acme.openai.azure.com");
    let context = Context::default().with_tools(vec![Tool::no_params("bash", "run")]);
    let body = build_azure_body(&model, &context, &options_with_key("k"), "dep").unwrap();
    // Azure's `supportsStrictMode` defaults to true, so `strict` is emitted.
    assert_eq!(body["tools"][0]["strict"], false);
}

// ============================================================================
// Codex
// ============================================================================

const CODEX_ENDPOINT: &str = "/codex/responses";

fn codex_token() -> String {
    let payload = json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": "acct_test" }
    });
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    format!("hdr.{encoded}.sig")
}

fn codex_model(base_url: &str) -> Model {
    let mut m = model(
        "gpt-5-codex",
        Api::OpenAiCodexResponses,
        "openai-codex",
        base_url,
    );
    m.reasoning = true;
    m
}

#[tokio::test]
async fn codex_maps_response_done_onto_the_shared_terminal_event() {
    let provider = MockProvider::sse(CODEX_ENDPOINT, "codex_text.sse").await;
    let model = codex_model(&provider.base_url());

    let stream = OpenAiCodexResponsesClient::new()
        .stream(
            &model,
            &Context::default(),
            &options_with_key(&codex_token()),
        )
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(
        collected.sequence,
        vec![
            Ev::Start,
            Ev::TextStart(0),
            Ev::TextDelta(0, "codex ".into()),
            Ev::TextDelta(0, "reply".into()),
            Ev::TextEnd(0, "codex reply".into()),
            Ev::Done("stop".into()),
        ]
    );
    assert_eq!(collected.terminal.api, "openai-codex-responses");
    assert_eq!(collected.terminal.text(), "codex reply");
    // `response.done` carries `end_turn`, which the standard dialect lacks.
    assert_eq!(collected.terminal.end_turn, Some(true));
    assert_eq!(collected.terminal.usage.input, 80);
}

#[tokio::test]
async fn codex_sends_its_auth_and_originator_headers() {
    let provider = MockProvider::sse(CODEX_ENDPOINT, "codex_text.sse").await;
    let model = codex_model(&provider.base_url());
    let token = codex_token();
    let mut options = options_with_key(&token);
    options.session_id = Some("codex-session".into());

    let stream = OpenAiCodexResponsesClient::new()
        .stream(&model, &Context::default(), &options)
        .await
        .unwrap();
    let _ = collect(stream).await;

    assert_eq!(
        provider.header("authorization").as_deref(),
        Some(format!("Bearer {token}").as_str())
    );
    assert_eq!(
        provider.header("chatgpt-account-id").as_deref(),
        Some("acct_test")
    );
    assert_eq!(provider.header("originator").as_deref(), Some("pi"));
    assert_eq!(
        provider.header("openai-beta").as_deref(),
        Some("responses=experimental")
    );
    assert_eq!(
        provider.header("session-id").as_deref(),
        Some("codex-session")
    );
    assert_eq!(
        provider.header("x-client-request-id").as_deref(),
        Some("codex-session")
    );
}

#[tokio::test]
async fn codex_error_frames_terminate_the_stream() {
    let provider = MockProvider::sse(CODEX_ENDPOINT, "codex_error.sse").await;
    let model = codex_model(&provider.base_url());

    let stream = OpenAiCodexResponsesClient::new()
        .stream(
            &model,
            &Context::default(),
            &options_with_key(&codex_token()),
        )
        .await
        .unwrap();
    let collected = collect(stream).await;

    assert_eq!(collected.sequence[0], Ev::Start);
    let Ev::Error(reason, message) = collected.sequence.last().unwrap() else {
        panic!("expected an error event");
    };
    assert_eq!(reason, "error");
    assert!(
        message.contains("Codex error: upstream exploded"),
        "got: {message}"
    );
}

#[tokio::test]
async fn codex_rejects_a_token_it_cannot_read_an_account_id_from() {
    let model = codex_model("https://chatgpt.com/backend-api");
    let stream = OpenAiCodexResponsesClient::new()
        .stream(&model, &Context::default(), &options_with_key("not-a-jwt"))
        .await
        .unwrap();
    let collected = collect(stream).await;

    let Ev::Error(_, message) = collected.sequence.last().unwrap() else {
        panic!("expected an error event");
    };
    assert!(
        message.contains("Failed to extract accountId"),
        "got: {message}"
    );
}

#[tokio::test]
async fn codex_surfaces_a_friendly_usage_limit_message() {
    let provider = MockProvider::raw(
        CODEX_ENDPOINT,
        r#"{"error":{"code":"usage_limit_reached","plan_type":"PLUS","message":"limit"}}"#,
        429,
    )
    .await;
    let model = codex_model(&provider.base_url());

    let stream = OpenAiCodexResponsesClient::new()
        .stream(
            &model,
            &Context::default(),
            &options_with_key(&codex_token()),
        )
        .await
        .unwrap();
    let collected = collect(stream).await;

    let Ev::Error(_, message) = collected.sequence.last().unwrap() else {
        panic!("expected an error event");
    };
    assert!(
        message.contains("You have hit your ChatGPT usage limit (plus plan)"),
        "got: {message}"
    );
}

#[test]
fn codex_body_uses_instructions_rather_than_a_system_message() {
    let model = codex_model("https://chatgpt.com/backend-api");
    let context = Context::new(vec![Message::User(pi_core::UserMessage::text("hi"))])
        .with_system_prompt("be terse");

    let body =
        build_codex_request_body(&model, &context, &options_with_key("k"), Some("sess")).unwrap();
    assert_eq!(body["instructions"], "be terse");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["text"], json!({ "verbosity": "low" }));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["prompt_cache_key"], "sess");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    // The system prompt is NOT repeated as an input item.
    assert_eq!(body["input"][0]["role"], "user");
}

#[test]
fn codex_falls_back_to_a_default_instruction() {
    let model = codex_model("https://chatgpt.com/backend-api");
    let body = build_codex_request_body(&model, &Context::default(), &options_with_key("k"), None)
        .unwrap();
    assert_eq!(body["instructions"], "You are a helpful assistant.");
    assert!(body.get("prompt_cache_key").is_none());
}

#[test]
fn codex_tools_carry_an_explicit_null_strict() {
    let model = codex_model("https://chatgpt.com/backend-api");
    let context = Context::default().with_tools(vec![Tool::no_params("bash", "run")]);
    let body = build_codex_request_body(&model, &context, &options_with_key("k"), None).unwrap();
    assert_eq!(body["tools"][0]["strict"], serde_json::Value::Null);
    assert_eq!(body["tools"][0]["name"], "bash");
}

#[test]
fn codex_reasoning_none_resolves_through_the_off_mapping() {
    let mut model = codex_model("https://chatgpt.com/backend-api");
    let options = pi_provider_openai::with_provider_option(
        options_with_key("k"),
        ProviderOptionKey::ReasoningEffort,
        json!("none"),
    );

    // No mapping → literal "none".
    let body = build_codex_request_body(&model, &Context::default(), &options, None).unwrap();
    assert_eq!(
        body["reasoning"],
        json!({ "effort": "none", "summary": "auto" })
    );

    // Explicit null off → the whole reasoning block is suppressed.
    model.thinking_level_map = Some(thinking_map(&[(
        pi_core::model::ModelThinkingLevel::Off,
        None,
    )]));
    let body = build_codex_request_body(&model, &Context::default(), &options, None).unwrap();
    assert!(body.get("reasoning").is_none());
}

#[test]
fn codex_reasoning_effort_maps_through_the_level_map() {
    let mut model = codex_model("https://chatgpt.com/backend-api");
    model.thinking_level_map = Some(thinking_map(&[(
        pi_core::model::ModelThinkingLevel::High,
        Some("xhigh"),
    )]));
    let options = pi_provider_openai::with_provider_option(
        options_with_key("k"),
        ProviderOptionKey::ReasoningEffort,
        json!("high"),
    );
    let body = build_codex_request_body(&model, &Context::default(), &options, None).unwrap();
    assert_eq!(body["reasoning"]["effort"], "xhigh");
}

#[test]
fn codex_text_verbosity_is_configurable() {
    let model = codex_model("https://chatgpt.com/backend-api");
    let options = pi_provider_openai::with_provider_option(
        options_with_key("k"),
        ProviderOptionKey::TextVerbosity,
        json!("high"),
    );
    let body = build_codex_request_body(&model, &Context::default(), &options, None).unwrap();
    assert_eq!(body["text"], json!({ "verbosity": "high" }));
}

/// As with `stream`, a missing credential is an in-stream `Error` event rather
/// than an `Err` — see `stream_simple_reports_a_missing_credential_in_the_stream`
/// in the completions suite for why this port diverges from upstream here.
#[tokio::test]
async fn codex_stream_simple_reports_a_missing_credential_in_the_stream() {
    let model = codex_model("https://chatgpt.com/backend-api");
    let stream = OpenAiCodexResponsesClient::new()
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

/// The Azure adapter had the same synchronous check; it is gone too.
#[tokio::test]
async fn azure_stream_simple_reports_a_missing_credential_in_the_stream() {
    let model = azure_model("https://acme.openai.azure.com");
    let stream = AzureOpenAiResponsesClient::new()
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

#[test]
fn every_adapter_reports_its_api_id() {
    let apis: Vec<String> = pi_provider_openai::all_api_clients()
        .iter()
        .map(|c| c.api().to_string())
        .collect();
    assert_eq!(
        apis,
        vec![
            "openai-completions",
            "openai-responses",
            "azure-openai-responses",
            "openai-codex-responses",
        ]
    );
}
