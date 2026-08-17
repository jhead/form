//! `buildParams` from both adapters. The two upstream copies are identical, so
//! the only parameter here is `gemma4_aware`, which the Gemini adapter sets and
//! the Vertex one does not (upstream's Vertex copy has no Gemma branch).

use pi_core::model::Model;
use pi_core::options::StreamOptions;
use pi_core::tool::Context;
use pi_core::AiError;

use crate::google_shared::{
    convert_messages, convert_tools, is_gemini3_flash_model, is_gemini3_pro_model, is_gemma4_model,
    resolve_google_function_calling_mode, supports_google_strict_tool_sampling,
};
use crate::options::GoogleOptions;
use crate::wire::{
    Content, FunctionCallingConfig, GenerateContentRequest, GenerationConfig, GoogleThinkingLevel,
    Part, ThinkingConfig, ToolConfig,
};

/// Gemini 3 Pro cannot disable thinking and Gemini 3 Flash/Flash-Lite cannot
/// either, so "off" becomes the lowest supported `thinkingLevel` *without*
/// `includeThoughts` — hidden thinking stays invisible to pi. Gemini 2.x still
/// takes `thinkingBudget: 0`.
fn disabled_thinking_config(model_id: &str, gemma4_aware: bool) -> ThinkingConfig {
    if is_gemini3_pro_model(model_id) {
        return ThinkingConfig {
            thinking_level: Some(GoogleThinkingLevel::Low),
            ..Default::default()
        };
    }
    if is_gemini3_flash_model(model_id) || (gemma4_aware && is_gemma4_model(model_id)) {
        return ThinkingConfig {
            thinking_level: Some(GoogleThinkingLevel::Minimal),
            ..Default::default()
        };
    }
    ThinkingConfig {
        thinking_budget: Some(0),
        ..Default::default()
    }
}

pub(crate) fn build_request_body(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    google: &GoogleOptions,
    gemma4_aware: bool,
) -> Result<GenerateContentRequest, AiError> {
    if options.request.is_aborted() {
        return Err(AiError::Aborted);
    }

    let contents = convert_messages(model, context);

    let mut generation_config = GenerationConfig {
        temperature: options.temperature,
        max_output_tokens: options.max_tokens,
        ..Default::default()
    };
    generation_config
        .extra
        .extend(google.generation_config.clone());

    let supports_strict_mode = supports_google_strict_tool_sampling(&model.id);
    let tools = context.tools();
    let function_calling_mode = if tools.is_empty() {
        None
    } else {
        resolve_google_function_calling_mode(
            tools,
            google.tool_choice.map(|c| c.as_str()),
            supports_strict_mode,
        )?
    };

    let mut wire_tools = convert_tools(tools, false, supports_strict_mode)?;
    if !google.extra_tools.is_empty() {
        wire_tools
            .get_or_insert_with(Vec::new)
            .extend(google.extra_tools.clone());
    }

    if google.thinking.as_ref().is_some_and(|t| t.enabled) && model.reasoning {
        let thinking = google.thinking.as_ref().expect("checked above");
        let mut config = ThinkingConfig {
            include_thoughts: Some(true),
            ..Default::default()
        };
        if let Some(level) = thinking.level {
            config.thinking_level = Some(level);
        } else if let Some(budget) = thinking.budget_tokens {
            config.thinking_budget = Some(budget);
        }
        generation_config.thinking_config = Some(config);
    } else if model.reasoning && google.thinking.is_some() {
        generation_config.thinking_config = Some(disabled_thinking_config(&model.id, gemma4_aware));
    }

    Ok(GenerateContentRequest {
        contents,
        system_instruction: context
            .system_prompt
            .as_ref()
            .filter(|prompt| !prompt.is_empty())
            // `tContent` wraps a bare string as a user-role Content.
            .map(|prompt| Content::new("user", vec![Part::text(prompt.clone())])),
        tools: wire_tools,
        tool_config: function_calling_mode.map(|mode| ToolConfig {
            function_calling_config: FunctionCallingConfig { mode },
        }),
        safety_settings: google.safety_settings.clone(),
        cached_content: google.cached_content.clone(),
        generation_config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{GoogleThinking, GoogleToolChoice};
    use crate::wire::FunctionCallingConfigMode;
    use pi_core::message::{Message, UserMessage};
    use pi_core::model::Api;
    use pi_core::tool::Tool;

    fn model(id: &str) -> Model {
        let mut m = Model::new(id, Api::GoogleGenerativeAi, "google", "https://example.com");
        m.reasoning = true;
        m
    }

    fn context() -> Context {
        Context::new(vec![Message::User(UserMessage::text("hi"))])
    }

    fn options() -> StreamOptions {
        StreamOptions::default()
    }

    #[test]
    fn minimal_body_matches_the_sdk_shape() {
        let google = GoogleOptions::default();
        let body = build_request_body(
            &model("gemini-2.5-flash"),
            &context(),
            &options(),
            &google,
            true,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "generationConfig": {}
            })
        );
    }

    #[test]
    fn system_prompt_becomes_a_user_role_content() {
        let google = GoogleOptions::default();
        let context = context().with_system_prompt("be brief");
        let body = build_request_body(
            &model("gemini-2.5-flash"),
            &context,
            &options(),
            &google,
            true,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&body).unwrap()["systemInstruction"],
            serde_json::json!({"role": "user", "parts": [{"text": "be brief"}]})
        );
    }

    #[test]
    fn thinking_disabled_uses_budget_zero_on_gemini_2() {
        let google = GoogleOptions {
            thinking: Some(GoogleThinking::disabled()),
            ..Default::default()
        };
        let body = build_request_body(
            &model("gemini-2.5-flash"),
            &context(),
            &options(),
            &google,
            true,
        )
        .unwrap();
        assert_eq!(
            body.generation_config.thinking_config,
            Some(ThinkingConfig {
                thinking_budget: Some(0),
                ..Default::default()
            })
        );
    }

    #[test]
    fn thinking_disabled_falls_back_to_lowest_level_on_gemini_3() {
        let google = GoogleOptions {
            thinking: Some(GoogleThinking::disabled()),
            ..Default::default()
        };
        let pro = build_request_body(
            &model("gemini-3.1-pro-preview"),
            &context(),
            &options(),
            &google,
            true,
        )
        .unwrap();
        assert_eq!(
            pro.generation_config
                .thinking_config
                .unwrap()
                .thinking_level,
            Some(GoogleThinkingLevel::Low)
        );
        let flash = build_request_body(
            &model("gemini-3-flash-preview"),
            &context(),
            &options(),
            &google,
            true,
        )
        .unwrap();
        let flash_config = flash.generation_config.thinking_config.unwrap();
        assert_eq!(
            flash_config.thinking_level,
            Some(GoogleThinkingLevel::Minimal)
        );
        // `includeThoughts` stays unset so hidden thinking is not surfaced.
        assert_eq!(flash_config.include_thoughts, None);
    }

    #[test]
    fn gemma4_only_gets_the_minimal_fallback_on_the_gemini_adapter() {
        let google = GoogleOptions {
            thinking: Some(GoogleThinking::disabled()),
            ..Default::default()
        };
        let aware =
            build_request_body(&model("gemma-4-27b"), &context(), &options(), &google, true)
                .unwrap();
        assert_eq!(
            aware
                .generation_config
                .thinking_config
                .unwrap()
                .thinking_level,
            Some(GoogleThinkingLevel::Minimal)
        );
        let vertex = build_request_body(
            &model("gemma-4-27b"),
            &context(),
            &options(),
            &google,
            false,
        )
        .unwrap();
        assert_eq!(
            vertex
                .generation_config
                .thinking_config
                .unwrap()
                .thinking_budget,
            Some(0)
        );
    }

    #[test]
    fn thinking_ignored_for_non_reasoning_models() {
        let mut m = model("gemini-2.5-flash");
        m.reasoning = false;
        let google = GoogleOptions {
            thinking: Some(GoogleThinking::with_budget(2048)),
            ..Default::default()
        };
        let body = build_request_body(&m, &context(), &options(), &google, true).unwrap();
        assert!(body.generation_config.thinking_config.is_none());
    }

    #[test]
    fn tools_become_function_declarations_with_a_tool_config() {
        let google = GoogleOptions {
            tool_choice: Some(GoogleToolChoice::Any),
            ..Default::default()
        };
        let context = context().with_tools(vec![Tool::no_params("bash", "run a command")]);
        let body = build_request_body(
            &model("gemini-2.5-flash"),
            &context,
            &options(),
            &google,
            true,
        )
        .unwrap();
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["tools"][0]["functionDeclarations"][0]["name"], "bash");
        assert_eq!(json["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            body.tool_config.unwrap().function_calling_config.mode,
            FunctionCallingConfigMode::Any
        );
    }

    #[test]
    fn no_tool_config_without_tools_or_a_choice() {
        let context = context().with_tools(vec![Tool::no_params("bash", "run a command")]);
        let body = build_request_body(
            &model("gemini-2.5-flash"),
            &context,
            &options(),
            &GoogleOptions::default(),
            true,
        )
        .unwrap();
        assert!(body.tool_config.is_none());
    }

    #[test]
    fn extension_options_reach_the_payload() {
        let mut google = GoogleOptions {
            safety_settings: Some(serde_json::json!([
                {"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "BLOCK_NONE"}
            ])),
            cached_content: Some("cachedContents/abc".into()),
            extra_tools: vec![serde_json::json!({"googleSearch": {}})],
            ..Default::default()
        };
        google
            .generation_config
            .insert("responseMimeType".into(), "application/json".into());
        google.generation_config.insert(
            "responseJsonSchema".into(),
            serde_json::json!({"type": "object"}),
        );
        let context = context().with_tools(vec![Tool::no_params("bash", "run a command")]);
        let json = serde_json::to_value(
            build_request_body(
                &model("gemini-2.5-flash"),
                &context,
                &options(),
                &google,
                true,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["safetySettings"][0]["threshold"], "BLOCK_NONE");
        assert_eq!(json["cachedContent"], "cachedContents/abc");
        assert_eq!(
            json["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            json["generationConfig"]["responseJsonSchema"]["type"],
            "object"
        );
        assert!(json["tools"][1]["googleSearch"].is_object());
    }
}
