//! Port of `api/google-generative-ai.ts`.
//!
//! Upstream drives `@google/genai`; this speaks the same HTTP:
//! `POST {baseUrl}/models/{model}:streamGenerateContent?alt=sse` with the key in
//! `x-goog-api-key`. `model.baseUrl` already carries the API version, which is
//! why upstream sets the SDK's `apiVersion` to `""`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use pi_core::api::ApiClient;
use pi_core::model::{Model, ModelThinkingLevel, ThinkingBudgets};
use pi_core::options::{SimpleStreamOptions, StreamOptions};
use pi_core::tool::Context;
use pi_core::{AiError, AssistantMessageEventStream};
use pi_http::client::HttpClient;

use crate::google_shared::{
    is_gemini3_flash_model, is_gemini3_pro_model, is_gemma4_model, ClampedThinkingLevel,
};
use crate::options::{GoogleOptions, GoogleStreamOptionsExt, GoogleThinking};
use crate::params::build_request_body;
use crate::stream::{start_stream, GoogleHttpRequest};
use crate::wire::GoogleThinkingLevel;
use pi_provider_common::simple_options::build_base_options;

pub const API: &str = "google-generative-ai";
const STREAM_ENDED_MESSAGE: &str = "Google stream ended without a finish reason";

/// The `google-generative-ai` adapter.
#[derive(Clone)]
pub struct GoogleGenerativeAiClient {
    http: Arc<HttpClient>,
}

impl Default for GoogleGenerativeAiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleGenerativeAiClient {
    pub fn new() -> Self {
        Self {
            http: HttpClient::shared(),
        }
    }

    pub fn with_http_client(http: Arc<HttpClient>) -> Self {
        Self { http }
    }

    fn request_url(model: &Model) -> String {
        format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            model.base_url.trim_end_matches('/'),
            model.id
        )
    }

    fn headers(model: &Model, options: &StreamOptions) -> BTreeMap<String, String> {
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("Content-Type".into(), "application/json".into());
        if let Some(model_headers) = &model.headers {
            headers.extend(model_headers.clone());
        }
        if let Some(api_key) = &options.request.api_key {
            headers.insert("x-goog-api-key".into(), api_key.clone());
        }
        pi_http::merge_headers(headers, &options.request.headers)
    }

    fn build(
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<GoogleHttpRequest, AiError> {
        let api_key = options
            .request
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty());
        if api_key.is_none() {
            return Err(AiError::auth(format!(
                "No API key for provider: {}",
                model.provider
            )));
        }
        let google = GoogleOptions::from_stream_options(options);
        let body = build_request_body(model, context, options, &google, true)?;
        Ok(GoogleHttpRequest {
            url: Self::request_url(model),
            headers: Self::headers(model, options),
            body: serde_json::to_value(body)
                .map_err(|err| AiError::protocol(format!("cannot serialize request: {err}")))?,
        })
    }
}

#[async_trait]
impl ApiClient for GoogleGenerativeAiClient {
    fn api(&self) -> &str {
        API
    }

    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let (model, context, options) = (model.clone(), context.clone(), options.clone());
        let build = {
            let (model, context, options) = (model.clone(), context, options.clone());
            Box::pin(async move { Self::build(&model, &context, &options) })
        };
        Ok(start_stream(
            API,
            STREAM_ENDED_MESSAGE,
            self.http.clone(),
            model,
            options,
            build,
        ))
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let stream_options = build_base_options(model, context, options, None);
        let thinking = resolve_thinking(model, options);
        self.stream(
            model,
            context,
            &stream_options.with_google_thinking(thinking),
        )
        .await
    }
}

/// Port of `streamSimple`'s thinking branch.
fn resolve_thinking(model: &Model, options: &SimpleStreamOptions) -> GoogleThinking {
    let Some(reasoning) = options.reasoning else {
        return GoogleThinking::disabled();
    };
    let clamped = model.clamp_thinking_level(ModelThinkingLevel::from(reasoning));
    let effort = ClampedThinkingLevel::from_model_level(clamped);

    if is_gemini3_pro_model(&model.id)
        || is_gemini3_flash_model(&model.id)
        || is_gemma4_model(&model.id)
    {
        return GoogleThinking::with_level(thinking_level_for(effort, &model.id));
    }
    GoogleThinking::with_budget(google_budget(
        &model.id,
        effort,
        options.thinking_budgets.as_ref(),
    ))
}

/// Port of `getThinkingLevel` (the Gemini copy, which knows about Gemma 4).
fn thinking_level_for(effort: ClampedThinkingLevel, model_id: &str) -> GoogleThinkingLevel {
    if is_gemini3_pro_model(model_id) {
        return match effort {
            ClampedThinkingLevel::Minimal | ClampedThinkingLevel::Low => GoogleThinkingLevel::Low,
            ClampedThinkingLevel::Medium | ClampedThinkingLevel::High => GoogleThinkingLevel::High,
        };
    }
    if is_gemma4_model(model_id) {
        return match effort {
            ClampedThinkingLevel::Minimal | ClampedThinkingLevel::Low => {
                GoogleThinkingLevel::Minimal
            }
            ClampedThinkingLevel::Medium | ClampedThinkingLevel::High => GoogleThinkingLevel::High,
        };
    }
    match effort {
        ClampedThinkingLevel::Minimal => GoogleThinkingLevel::Minimal,
        ClampedThinkingLevel::Low => GoogleThinkingLevel::Low,
        ClampedThinkingLevel::Medium => GoogleThinkingLevel::Medium,
        ClampedThinkingLevel::High => GoogleThinkingLevel::High,
    }
}

/// Port of `getGoogleBudget` (the Gemini copy, which has a Flash-Lite table).
fn google_budget(
    model_id: &str,
    effort: ClampedThinkingLevel,
    custom: Option<&ThinkingBudgets>,
) -> i64 {
    if let Some(budget) = custom.and_then(|budgets| effort.budget_from(budgets)) {
        return budget as i64;
    }
    let table: [i64; 4] = if model_id.contains("2.5-pro") {
        [128, 2048, 8192, 32768]
    } else if model_id.contains("2.5-flash-lite") {
        [512, 2048, 8192, 24576]
    } else if model_id.contains("2.5-flash") {
        [128, 2048, 8192, 24576]
    } else {
        // Dynamic budget.
        return -1;
    };
    match effort {
        ClampedThinkingLevel::Minimal => table[0],
        ClampedThinkingLevel::Low => table[1],
        ClampedThinkingLevel::Medium => table[2],
        ClampedThinkingLevel::High => table[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::{Message, UserMessage};
    use pi_core::model::{Api, ThinkingLevel};

    fn model(id: &str) -> Model {
        let mut m = Model::new(
            id,
            Api::GoogleGenerativeAi,
            "google",
            "https://generativelanguage.googleapis.com/v1beta",
        );
        m.reasoning = true;
        m
    }

    fn simple(reasoning: Option<ThinkingLevel>) -> SimpleStreamOptions {
        SimpleStreamOptions {
            reasoning,
            ..Default::default()
        }
    }

    #[test]
    fn url_matches_the_sdk_path() {
        assert_eq!(
            GoogleGenerativeAiClient::request_url(&model("gemini-2.5-flash")),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn api_key_goes_in_the_goog_header() {
        let mut options = StreamOptions::default();
        options.request.api_key = Some("k".into());
        let headers = GoogleGenerativeAiClient::headers(&model("gemini-2.5-flash"), &options);
        assert_eq!(headers.get("x-goog-api-key").unwrap(), "k");
    }

    #[test]
    fn header_overrides_can_remove_a_default() {
        let mut options = StreamOptions::default();
        options.request.api_key = Some("k".into());
        options
            .request
            .headers
            .insert("x-goog-api-key".into(), None);
        let headers = GoogleGenerativeAiClient::headers(&model("gemini-2.5-flash"), &options);
        assert!(!headers.contains_key("x-goog-api-key"));
    }

    #[test]
    fn missing_api_key_is_an_auth_error() {
        let err = GoogleGenerativeAiClient::build(
            &model("gemini-2.5-flash"),
            &Context::new(vec![Message::User(UserMessage::text("hi"))]),
            &StreamOptions::default(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "auth");
        assert!(err.message().contains("No API key for provider: google"));
    }

    #[test]
    fn no_reasoning_disables_thinking() {
        let thinking = resolve_thinking(&model("gemini-2.5-flash"), &simple(None));
        assert_eq!(thinking, GoogleThinking::disabled());
    }

    #[test]
    fn gemini_25_uses_token_budgets() {
        assert_eq!(
            resolve_thinking(&model("gemini-2.5-pro"), &simple(Some(ThinkingLevel::High))),
            GoogleThinking::with_budget(32768)
        );
        assert_eq!(
            resolve_thinking(
                &model("gemini-2.5-flash-lite"),
                &simple(Some(ThinkingLevel::Minimal))
            ),
            GoogleThinking::with_budget(512)
        );
        assert_eq!(
            resolve_thinking(
                &model("gemini-2.5-flash"),
                &simple(Some(ThinkingLevel::Minimal))
            ),
            GoogleThinking::with_budget(128)
        );
        // xhigh/max clamp to high.
        assert_eq!(
            resolve_thinking(&model("gemini-2.5-pro"), &simple(Some(ThinkingLevel::Max))),
            GoogleThinking::with_budget(32768)
        );
    }

    #[test]
    fn unknown_models_get_a_dynamic_budget() {
        assert_eq!(
            resolve_thinking(&model("gemini-1.5-pro"), &simple(Some(ThinkingLevel::Low))),
            GoogleThinking::with_budget(-1)
        );
    }

    #[test]
    fn custom_budgets_win() {
        let options = SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Low),
            thinking_budgets: Some(ThinkingBudgets {
                low: Some(999),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_thinking(&model("gemini-2.5-pro"), &options),
            GoogleThinking::with_budget(999)
        );
    }

    #[test]
    fn gemini3_and_gemma4_use_thinking_levels() {
        assert_eq!(
            resolve_thinking(
                &model("gemini-3-pro-preview"),
                &simple(Some(ThinkingLevel::Minimal))
            ),
            GoogleThinking::with_level(GoogleThinkingLevel::Low)
        );
        assert_eq!(
            resolve_thinking(
                &model("gemini-3-pro-preview"),
                &simple(Some(ThinkingLevel::Medium))
            ),
            GoogleThinking::with_level(GoogleThinkingLevel::High)
        );
        assert_eq!(
            resolve_thinking(
                &model("gemini-3-flash-preview"),
                &simple(Some(ThinkingLevel::Medium))
            ),
            GoogleThinking::with_level(GoogleThinkingLevel::Medium)
        );
        assert_eq!(
            resolve_thinking(&model("gemma-4-27b"), &simple(Some(ThinkingLevel::Low))),
            GoogleThinking::with_level(GoogleThinkingLevel::Minimal)
        );
    }

    #[test]
    fn max_tokens_are_clamped_to_the_context_window() {
        let mut m = model("gemini-2.5-flash");
        m.context_window = 10_000;
        m.max_tokens = 8_192;
        let context = Context::new(vec![Message::User(UserMessage::text("hi"))]);
        let options = build_base_options(&m, &context, &simple(None), None);
        // 10_000 - ~1 - 4096 leaves less than the model cap.
        assert_eq!(options.max_tokens, Some(5_903));
    }

    /// The estimate is in UTF-16 code units, as JavaScript's `String.length` is.
    ///
    /// The copy this adapter used to carry counted UTF-8 bytes, so a CJK prompt
    /// was over-counted by roughly 3x and `max_tokens` came out far too small.
    #[test]
    fn max_tokens_are_sized_from_utf16_code_units() {
        let prompt = "日本語🙈🙉🙉🙈café".repeat(100);
        assert_eq!(prompt.encode_utf16().count(), 1_500);
        assert_eq!(prompt.len(), 3_000);

        let mut m = model("gemini-2.5-flash");
        m.context_window = 10_000;
        m.max_tokens = 8_192;
        let context = Context::new(vec![Message::User(UserMessage::text(prompt))]);
        let options = build_base_options(&m, &context, &simple(None), None);
        // 10_000 - ceil(1500 / 4) - 4096 safety = 5_529.
        assert_eq!(options.max_tokens, Some(5_529));
        // The abandoned byte count would have left 5_154.
        assert_ne!(options.max_tokens, Some(5_154));
    }
}
