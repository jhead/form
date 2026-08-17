//! Port of `test/supports-xhigh.test.ts` and the catalog-driven cases in
//! `test/max-thinking.test.ts`.
//!
//! These run entirely against the embedded catalog — the point of the upstream
//! suite is that the *data* encodes each model's real effort options.

use pi_catalog::{
    builtin_model, clamp_thinking_level, get_supported_thinking_levels, supports_thinking,
};
use pi_core::{Api, Model, ModelThinkingLevel as L};

fn model(provider: &str, id: &str) -> Model {
    builtin_model(provider, id).unwrap_or_else(|| panic!("missing model {provider}/{id}"))
}

fn levels(provider: &str, id: &str) -> Vec<L> {
    get_supported_thinking_levels(&model(provider, id))
}

#[test]
fn anthropic_opus_4_6_includes_max_but_not_xhigh() {
    let l = levels("anthropic", "claude-opus-4-6");
    assert!(l.contains(&L::Max));
    assert!(!l.contains(&L::Xhigh));
}

#[test]
fn anthropic_opus_4_8_includes_xhigh_and_max() {
    let l = levels("anthropic", "claude-opus-4-8");
    assert!(l.contains(&L::Xhigh));
    assert!(l.contains(&L::Max));
}

#[test]
fn anthropic_opus_5_includes_xhigh_and_max() {
    let l = levels("anthropic", "claude-opus-5");
    assert!(l.contains(&L::Xhigh));
    assert!(l.contains(&L::Max));
}

#[test]
fn anthropic_sonnet_4_6_includes_max_but_not_xhigh() {
    let l = levels("anthropic", "claude-sonnet-4-6");
    assert!(l.contains(&L::Max));
    assert!(!l.contains(&L::Xhigh));
}

#[test]
fn anthropic_sonnet_5_includes_xhigh_and_max() {
    let l = levels("anthropic", "claude-sonnet-5");
    assert!(l.contains(&L::Xhigh));
    assert!(l.contains(&L::Max));
}

/// Fable is thinking-only: `off` is explicitly `null` in the catalog.
#[test]
fn anthropic_fable_5_includes_xhigh_and_max_but_not_off() {
    let l = levels("anthropic", "claude-fable-5");
    assert!(l.contains(&L::Xhigh));
    assert!(l.contains(&L::Max));
    assert!(!l.contains(&L::Off));
}

#[test]
fn claude_sonnet_4_5_has_neither_xhigh_nor_max() {
    let l = levels("anthropic", "claude-sonnet-4-5");
    assert!(!l.contains(&L::Xhigh));
    assert!(!l.contains(&L::Max));
}

#[test]
fn openai_codex_models_include_xhigh() {
    for id in [
        "gpt-5.4",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        assert!(
            levels("openai-codex", id).contains(&L::Xhigh),
            "openai-codex/{id}"
        );
    }
}

#[test]
fn openai_gpt_5_6_models_expose_the_full_ladder() {
    for id in ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
        assert_eq!(
            levels("openai", id),
            vec![L::Off, L::Low, L::Medium, L::High, L::Xhigh, L::Max],
            "openai/{id}"
        );
    }
}

#[test]
fn openai_gpt_5_5_pro_is_medium_high_xhigh_only() {
    assert_eq!(
        levels("openai", "gpt-5.5-pro"),
        vec![L::Medium, L::High, L::Xhigh]
    );
}

#[test]
fn openrouter_gpt_5_5_pro_is_medium_high_xhigh_only() {
    assert_eq!(
        levels("openrouter", "openai/gpt-5.5-pro"),
        vec![L::Medium, L::High, L::Xhigh]
    );
}

#[test]
fn deepseek_v4_flash_on_deepseek_is_off_low_high_max() {
    assert_eq!(
        levels("deepseek", "deepseek-v4-flash"),
        vec![L::Off, L::Low, L::High, L::Max]
    );
}

#[test]
fn deepseek_v4_flash_on_opencode_go_is_off_high_max() {
    assert_eq!(
        levels("opencode-go", "deepseek-v4-flash"),
        vec![L::Off, L::High, L::Max]
    );
}

#[test]
fn opencode_go_kimi_k2_6_is_off_high_only() {
    assert_eq!(levels("opencode-go", "kimi-k2.6"), vec![L::Off, L::High]);
}

#[test]
fn moonshot_kimi_k2_7_code_excludes_thinking_off() {
    for provider in ["moonshotai", "moonshotai-cn"] {
        assert_eq!(
            levels(provider, "kimi-k2.7-code"),
            vec![L::Minimal, L::Low, L::Medium, L::High],
            "{provider}"
        );
    }
}

#[test]
fn moonshot_kimi_k3_uses_verified_effort_options() {
    for provider in ["moonshotai", "moonshotai-cn"] {
        assert_eq!(
            levels(provider, "kimi-k3"),
            vec![L::Low, L::High, L::Max],
            "{provider}"
        );
    }
}

#[test]
fn kimi_coding_k3_is_low_high_max() {
    assert_eq!(levels("kimi-coding", "k3"), vec![L::Low, L::High, L::Max]);
}

#[test]
fn opencode_grok_build_is_high_only() {
    assert_eq!(levels("opencode", "grok-build-0.1"), vec![L::High]);
}

#[test]
fn openrouter_deepseek_v4_flash_is_off_high_xhigh() {
    assert_eq!(
        levels("openrouter", "deepseek/deepseek-v4-flash"),
        vec![L::Off, L::High, L::Xhigh]
    );
}

#[test]
fn openrouter_opus_4_6_includes_max_but_not_xhigh() {
    let l = levels("openrouter", "anthropic/claude-opus-4.6");
    assert!(l.contains(&L::Max));
    assert!(!l.contains(&L::Xhigh));
}

#[test]
fn bedrock_claude_opus_5_includes_xhigh_and_max() {
    let l = levels("amazon-bedrock", "global.anthropic.claude-opus-5");
    assert!(l.contains(&L::Xhigh));
    assert!(l.contains(&L::Max));
}

#[test]
fn bedrock_claude_fable_5_includes_xhigh_and_max_but_not_off() {
    let l = levels("amazon-bedrock", "global.anthropic.claude-fable-5");
    assert!(l.contains(&L::Xhigh));
    assert!(l.contains(&L::Max));
    assert!(!l.contains(&L::Off));
}

#[test]
fn xai_grok_4_6_has_xhigh_but_neither_off_nor_max() {
    assert_eq!(
        levels("xai", "grok-4.6"),
        vec![L::Low, L::Medium, L::High, L::Xhigh]
    );
}

// ---- max-thinking.test.ts ----

fn synthetic_reasoning_model() -> Model {
    let mut model = Model::new(
        "ordinary-reasoning",
        Api::OpenAiCompletions,
        "test",
        "https://example.com/v1",
    );
    model.reasoning = true;
    model
}

#[test]
fn max_is_opt_in_for_ordinary_reasoning_models() {
    let model = synthetic_reasoning_model();
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![L::Off, L::Minimal, L::Low, L::Medium, L::High]
    );
    assert_eq!(clamp_thinking_level(&model, L::Max), L::High);
}

#[test]
fn non_reasoning_models_support_only_off() {
    let model = Model::new("plain", Api::OpenAiCompletions, "test", "https://x.test");
    assert_eq!(get_supported_thinking_levels(&model), vec![L::Off]);
    assert!(!supports_thinking(&model));
    assert_eq!(clamp_thinking_level(&model, L::High), L::Off);
}

#[test]
fn codex_models_expose_xhigh_and_max_in_the_level_map() {
    for id in ["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"] {
        let model = model("openai-codex", id);
        let map = model.thinking_level_map.as_ref().expect("thinkingLevelMap");
        assert_eq!(map.get(&L::Xhigh), Some(&Some("xhigh".to_string())), "{id}");
        assert_eq!(map.get(&L::Max), Some(&Some("max".to_string())), "{id}");
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![
                L::Off,
                L::Minimal,
                L::Low,
                L::Medium,
                L::High,
                L::Xhigh,
                L::Max
            ],
            "{id}"
        );
    }
}

/// A `null` at `xhigh` with a value at `max` leaves a hole; clamping upward
/// must skip it rather than stop.
#[test]
fn a_hole_between_high_and_max_clamps_upward() {
    let mut model = synthetic_reasoning_model();
    model.id = "high-and-max".to_string();
    let mut map = pi_core::ThinkingLevelMap::new();
    map.insert(L::Xhigh, None);
    map.insert(L::Max, Some("max".to_string()));
    model.thinking_level_map = Some(map);

    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![L::Off, L::Minimal, L::Low, L::Medium, L::High, L::Max]
    );
    assert_eq!(clamp_thinking_level(&model, L::Xhigh), L::Max);
}

/// Clamping falls back downward when nothing above the request is supported.
#[test]
fn clamping_falls_back_downward_when_nothing_higher_exists() {
    // xAI Grok 4.6 has no `off` and no `max`.
    let grok = model("xai", "grok-4.6");
    assert_eq!(clamp_thinking_level(&grok, L::Max), L::Xhigh);
    assert_eq!(clamp_thinking_level(&grok, L::Off), L::Low);
    assert_eq!(clamp_thinking_level(&grok, L::Minimal), L::Low);
}

#[test]
fn clamping_a_supported_level_is_the_identity() {
    let model = model("anthropic", "claude-opus-5");
    assert_eq!(clamp_thinking_level(&model, L::High), L::High);
    assert_eq!(clamp_thinking_level(&model, L::Max), L::Max);
}
