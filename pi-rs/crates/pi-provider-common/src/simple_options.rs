//! Port of `api/simple-options.ts`.
//!
//! `stream_simple` takes unified options; every adapter turns them into its own
//! raw [`StreamOptions`] through [`build_base_options`], and sizes `max_tokens`
//! through [`clamp_max_tokens_to_context`].
//!
//! # The estimator this used to carry
//!
//! Upstream's `clampMaxTokensToContext` calls `estimateContextTokens`, which
//! counts `String.length` — i.e. **UTF-16 code units**. Four adapters had each
//! ported that estimator privately and no two agreed:
//!
//! | copy | unit | correct? |
//! |---|---|---|
//! | anthropic, misc | `chars().count()` (Unicode scalar values) | no |
//! | openai, google | `len()` (UTF-8 bytes) | no |
//! | [`pi_http::estimate`] | `encode_utf16().count()` | yes |
//!
//! For ASCII all three agree, which is why this survived. For anything else
//! they diverge in opposite directions — `chars()` under-counts astral-plane
//! text by 2x, `len()` over-counts CJK by 3x — and `max_tokens` was sized from
//! the wrong number in three of the four adapters. There is exactly one
//! estimator now and it is `pi_http::estimate`; see
//! `estimator_counts_utf16_code_units_like_javascript` below.

use pi_core::model::{Model, ThinkingBudgets, ThinkingLevel};
use pi_core::options::{SimpleStreamOptions, StreamOptions};
use pi_core::tool::Context;

use pi_http::estimate::estimate_context_tokens;

const CONTEXT_SAFETY_TOKENS: i64 = 4096;
const MIN_MAX_TOKENS: u64 = 1;

/// Tokens always left for the answer when a thinking budget shares the ceiling.
pub const MIN_ANSWER_TOKENS: u64 = 1024;

/// Port of `clampMaxTokensToContext`.
///
/// Shrinks a requested `max_tokens` so the response still fits inside what is
/// left of the context window, with a fixed safety margin.
pub fn clamp_max_tokens_to_context(model: &Model, context: &Context, max_tokens: u64) -> u64 {
    if model.context_window == 0 {
        return max_tokens.max(MIN_MAX_TOKENS);
    }
    // `context_window` is an unsigned hard limit; the estimate is signed because
    // it can ride on corrective `Usage` records. Subtract in i128 so neither the
    // widening nor an unusually large window can wrap, then clamp back down.
    let used =
        i128::from(estimate_context_tokens(context).tokens) + i128::from(CONTEXT_SAFETY_TOKENS);
    let available = (i128::from(model.context_window) - used).max(i128::from(MIN_MAX_TOKENS));
    u64::try_from(available.min(i128::from(max_tokens))).unwrap_or(MIN_MAX_TOKENS)
}

/// Port of `buildBaseOptions`.
///
/// Per-request `sampling_params` are merged **over** `model.sampling_params`, so
/// caller keys win. `api_key` follows upstream's `apiKey || options?.apiKey`:
/// an explicit non-empty argument wins, otherwise whatever the options carry
/// stays. Everything else is a pass-through, because the Rust
/// `SimpleStreamOptions` already embeds `StreamOptions` rather than restating
/// each field the way the TypeScript interface does.
pub fn build_base_options(
    model: &Model,
    context: &Context,
    options: &SimpleStreamOptions,
    api_key: Option<&str>,
) -> StreamOptions {
    let mut base = options.stream.clone();

    if model.sampling_params.is_some() || base.sampling_params.is_some() {
        let mut merged = model.sampling_params.clone().unwrap_or_default();
        if let Some(request) = &base.sampling_params {
            for (key, value) in request {
                merged.insert(key.clone(), value.clone());
            }
        }
        base.sampling_params = Some(merged);
    }

    let requested = base.max_tokens.unwrap_or(model.max_tokens);
    base.max_tokens = Some(clamp_max_tokens_to_context(model, context, requested));

    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        base.request.api_key = Some(key.to_string());
    }
    base
}

/// Port of `clampReasoning`: `xhigh`/`max` collapse onto `high`, because the
/// token-budget table only has four rows.
pub fn clamp_reasoning(effort: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
    match effort {
        Some(ThinkingLevel::Xhigh) | Some(ThinkingLevel::Max) => Some(ThinkingLevel::High),
        other => other,
    }
}

/// Upstream's `defaultBudgets` table.
pub fn default_thinking_budgets() -> ThinkingBudgets {
    ThinkingBudgets {
        minimal: Some(1024),
        low: Some(2048),
        medium: Some(8192),
        high: Some(16384),
    }
}

/// Resolve a budget for `level`, with caller overrides layered on the defaults.
pub fn thinking_budget_for(level: ThinkingLevel, custom: Option<&ThinkingBudgets>) -> u64 {
    let defaults = default_thinking_budgets();
    let pick = |c: Option<u32>, d: Option<u32>| u64::from(c.or(d).unwrap_or(0));
    match clamp_reasoning(Some(level)).unwrap_or(ThinkingLevel::Medium) {
        ThinkingLevel::Minimal => pick(custom.and_then(|b| b.minimal), defaults.minimal),
        ThinkingLevel::Low => pick(custom.and_then(|b| b.low), defaults.low),
        ThinkingLevel::Medium => pick(custom.and_then(|b| b.medium), defaults.medium),
        _ => pick(custom.and_then(|b| b.high), defaults.high),
    }
}

/// The `{ maxTokens, thinkingBudget }` pair `adjustMaxTokensForThinking` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustedThinking {
    pub max_tokens: u64,
    pub thinking_budget: u64,
}

/// Port of `adjustMaxTokensForThinking`.
///
/// `base_max_tokens` of `None` means the caller set no explicit cap, so the
/// model ceiling is used and the thinking budget has to fit inside it.
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustedThinking {
    let mut thinking_budget = thinking_budget_for(reasoning_level, custom_budgets);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => base.saturating_add(thinking_budget).min(model_max_tokens),
    };
    if max_tokens <= thinking_budget {
        thinking_budget = max_tokens.saturating_sub(MIN_ANSWER_TOKENS);
    }
    AdjustedThinking {
        max_tokens,
        thinking_budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::{Message, UserMessage};
    use pi_core::model::Api;
    use serde_json::json;

    fn model() -> Model {
        Model::new("m", Api::OpenAiCompletions, "p", "https://x")
    }

    #[test]
    fn request_sampling_params_win_over_model_defaults() {
        let mut model = model();
        let mut model_params = serde_json::Map::new();
        model_params.insert("top_p".into(), json!(0.5));
        model_params.insert("top_k".into(), json!(40));
        model.sampling_params = Some(model_params);

        let mut options = SimpleStreamOptions::default();
        let mut request_params = serde_json::Map::new();
        request_params.insert("top_p".into(), json!(0.9));
        options.stream.sampling_params = Some(request_params);

        let base = build_base_options(&model, &Context::default(), &options, None);
        let merged = base.sampling_params.unwrap();
        assert_eq!(merged["top_p"], json!(0.9));
        assert_eq!(merged["top_k"], json!(40));
    }

    #[test]
    fn merges_model_sampling_params_under_caller_overrides() {
        let mut model = model();
        model.sampling_params = Some(json!({"topP": 0.5, "seed": 1}).as_object().unwrap().clone());
        let mut options = SimpleStreamOptions::default();
        options.stream.sampling_params = Some(json!({"topP": 0.9}).as_object().unwrap().clone());

        let base = build_base_options(&model, &Context::default(), &options, None);
        let sampling = base.sampling_params.unwrap();
        assert_eq!(sampling["topP"], 0.9);
        assert_eq!(sampling["seed"], 1);
    }

    #[test]
    fn absent_sampling_params_stay_absent() {
        let base = build_base_options(
            &model(),
            &Context::default(),
            &SimpleStreamOptions::default(),
            None,
        );
        assert!(base.sampling_params.is_none());
    }

    #[test]
    fn an_explicit_api_key_overrides_the_one_in_options() {
        let mut options = SimpleStreamOptions::default();
        options.stream.request.api_key = Some("from-options".into());
        let base = build_base_options(
            &model(),
            &Context::default(),
            &options,
            Some("from-argument"),
        );
        assert_eq!(base.request.api_key.as_deref(), Some("from-argument"));

        // Upstream is `apiKey || options?.apiKey`, so an empty argument falls through.
        let base = build_base_options(&model(), &Context::default(), &options, Some(""));
        assert_eq!(base.request.api_key.as_deref(), Some("from-options"));
    }

    #[test]
    fn max_tokens_is_clamped_to_the_remaining_context() {
        let mut model = model();
        model.context_window = 5000;
        model.max_tokens = 4096;
        let context = Context::new(vec![Message::User(UserMessage::text("x".repeat(400)))]);
        let base = build_base_options(&model, &context, &SimpleStreamOptions::default(), None);
        // 5000 - (100 est tokens) - 4096 safety = 804
        assert_eq!(base.max_tokens, Some(804));
    }

    #[test]
    fn clamps_to_the_remaining_window() {
        let mut model = model();
        model.context_window = 5000;
        let context = Context::default();
        // 5000 - 0 - 4096 = 904 tokens left.
        assert_eq!(clamp_max_tokens_to_context(&model, &context, 8000), 904);
        assert_eq!(clamp_max_tokens_to_context(&model, &context, 100), 100);
    }

    #[test]
    fn never_returns_zero() {
        let mut model = model();
        model.context_window = 10;
        assert_eq!(
            clamp_max_tokens_to_context(&model, &Context::default(), 8000),
            1
        );
    }

    #[test]
    fn a_zero_context_window_means_no_clamping() {
        let mut model = model();
        model.context_window = 0;
        assert_eq!(
            clamp_max_tokens_to_context(&model, &Context::default(), 8000),
            8000
        );
        assert_eq!(
            clamp_max_tokens_to_context(&model, &Context::default(), 0),
            1
        );
    }

    /// The bug this consolidation exists to fix.
    ///
    /// Upstream counts `String.length`, which is UTF-16 code units. The three
    /// private estimators counted Unicode scalar values (`chars().count()`) or
    /// UTF-8 bytes (`len()`) instead, so every non-ASCII prompt was sized from
    /// the wrong number and `clamp_max_tokens_to_context` inherited the error.
    #[test]
    fn estimator_counts_utf16_code_units_like_javascript() {
        // CJK: 1 scalar, 1 UTF-16 unit, 3 UTF-8 bytes each.
        // Emoji (astral): 1 scalar, 2 UTF-16 units, 4 UTF-8 bytes each.
        // Accented Latin (precomposed): 1 scalar, 1 UTF-16 unit, 2 UTF-8 bytes.
        // Mixed so that all three counts land in a *different* token bucket.
        let text = "日本語🙈🙉🙉🙈café";
        let scalars = text.chars().count() as i64;
        let utf16_units = text.encode_utf16().count() as i64;
        let utf8_bytes = text.len() as i64;
        assert_eq!((scalars, utf16_units, utf8_bytes), (11, 15, 30));

        let tokens_for = |chars: i64| chars.div_euclid(4) + i64::from(chars % 4 != 0);
        assert_eq!(
            (
                tokens_for(scalars),
                tokens_for(utf16_units),
                tokens_for(utf8_bytes)
            ),
            (3, 4, 8),
            "the three units must be distinguishable at token granularity"
        );

        // `estimate_text_tokens` is the primitive everything else is built on.
        assert_eq!(
            pi_http::estimate::estimate_text_tokens(text),
            tokens_for(utf16_units)
        );

        // And `clamp_max_tokens_to_context` inherits it, which is where the
        // wrong unit used to become an observably wrong request.
        let mut model = model();
        model.context_window = 1_000_000;
        model.max_tokens = 1_000_000;
        let context = Context::new(vec![Message::User(UserMessage::text(text))]);
        let clamped = clamp_max_tokens_to_context(&model, &context, u64::MAX);
        assert_eq!(
            clamped,
            (1_000_000 - tokens_for(utf16_units) - CONTEXT_SAFETY_TOKENS) as u64
        );
        for wrong in [scalars, utf8_bytes] {
            assert_ne!(
                clamped,
                (1_000_000 - tokens_for(wrong) - CONTEXT_SAFETY_TOKENS) as u64,
                "clamp must not agree with a {wrong}-unit count"
            );
        }
    }

    #[test]
    fn thinking_budget_leaves_room_for_the_answer() {
        let adjusted = adjust_max_tokens_for_thinking(None, 2048, ThinkingLevel::High, None);
        assert_eq!(adjusted.max_tokens, 2048);
        assert_eq!(adjusted.thinking_budget, 1024);
    }

    #[test]
    fn an_explicit_cap_makes_room_for_the_budget_up_to_the_model_ceiling() {
        let adjusted = adjust_max_tokens_for_thinking(Some(4096), 32_000, ThinkingLevel::Low, None);
        assert_eq!(adjusted.max_tokens, 4096 + 2048);
        assert_eq!(adjusted.thinking_budget, 2048);

        let capped = adjust_max_tokens_for_thinking(Some(4096), 5000, ThinkingLevel::Low, None);
        assert_eq!(capped.max_tokens, 5000);
        assert_eq!(capped.thinking_budget, 2048);
    }

    #[test]
    fn xhigh_and_max_collapse_onto_high() {
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Xhigh)),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Max)),
            Some(ThinkingLevel::High)
        );
        assert_eq!(clamp_reasoning(None), None);
        assert_eq!(
            thinking_budget_for(ThinkingLevel::Max, None),
            thinking_budget_for(ThinkingLevel::High, None)
        );
    }

    #[test]
    fn custom_budgets_override_the_defaults_level_by_level() {
        let custom = ThinkingBudgets {
            minimal: None,
            low: Some(99),
            medium: None,
            high: None,
        };
        assert_eq!(thinking_budget_for(ThinkingLevel::Low, Some(&custom)), 99);
        assert_eq!(
            thinking_budget_for(ThinkingLevel::Medium, Some(&custom)),
            8192
        );
    }
}
