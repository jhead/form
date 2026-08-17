use super::*;
use crate::protocol::{ThinkingLevel, Usage};

fn ref_for(provider_id: &str, model_id: &str) -> ModelRef {
    ModelRef {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        thinking_level: ThinkingLevel::Off,
    }
}

#[test]
fn catalog_parses_and_covers_f8_1() {
    let catalog = load();
    assert!(!catalog.generated_at.is_empty(), "data file needs a stamp");
    let ids: Vec<&str> = catalog.providers.iter().map(|p| p.id.as_str()).collect();
    for expected in [
        "anthropic",
        "openai",
        "google",
        "openrouter",
        "xai",
        "groq",
        "mistral",
        "deepseek",
        "ollama",
    ] {
        assert!(ids.contains(&expected), "missing provider {expected}");
    }
    assert!(
        catalog.provider("openrouter").unwrap().note.is_some(),
        "OpenRouter must carry the proxying note"
    );
    let ollama = catalog.provider("ollama").unwrap();
    assert_eq!(ollama.auth, vec![AuthMethod::None]);
    assert!(ollama.models.iter().all(|m| m.pricing.input == 0.0));
}

#[test]
fn every_model_resolves_by_ref() {
    for (provider, model) in load().models() {
        let model_ref = ref_for(&provider.id, &model.id);
        let resolved =
            resolve(&model_ref).unwrap_or_else(|| panic!("{}/{}", provider.id, model.id));
        assert_eq!(&resolved, model);
        assert!(resolve_ref(&model_ref).is_some());

        // And by string, which is what the CLI and the picker hand us.
        let parsed = parse_ref(&format!("{}/{}", provider.id, model.id))
            .unwrap_or_else(|| panic!("parse {}/{}", provider.id, model.id));
        assert_eq!(parsed.provider_id, provider.id);
        assert_eq!(parsed.model_id, model.id);
    }
}

#[test]
fn ids_are_unique() {
    let catalog = load();
    let mut provider_ids: Vec<&str> = catalog.providers.iter().map(|p| p.id.as_str()).collect();
    provider_ids.sort_unstable();
    let count = provider_ids.len();
    provider_ids.dedup();
    assert_eq!(provider_ids.len(), count, "duplicate provider id");

    for provider in &catalog.providers {
        let mut model_ids: Vec<&str> = provider.models.iter().map(|m| m.id.as_str()).collect();
        model_ids.sort_unstable();
        let count = model_ids.len();
        model_ids.dedup();
        assert_eq!(
            model_ids.len(),
            count,
            "duplicate model id in {}",
            provider.id
        );
    }
}

#[test]
fn pricing_and_windows_are_internally_consistent() {
    for (provider, model) in load().models() {
        let tag = format!("{}/{}", provider.id, model.id);
        let p = &model.pricing;
        assert!(model.context_window > 0, "{tag}: empty context window");
        assert!(
            model.max_output <= model.context_window,
            "{tag}: max output exceeds the window"
        );
        assert!(p.input >= 0.0 && p.output >= 0.0, "{tag}: negative price");
        assert!(
            p.output >= p.input,
            "{tag}: output is never cheaper than input"
        );
        assert!(p.cache_read <= p.input, "{tag}: cache read beats input");
        if !model.capabilities.caching {
            assert_eq!(
                p.cache_read, 0.0,
                "{tag}: priced cache read without caching"
            );
            assert_eq!(
                p.cache_write, 0.0,
                "{tag}: priced cache write without caching"
            );
        }
        assert!(
            model.capabilities.streaming,
            "{tag}: the app only speaks streaming"
        );
        assert!(!model.name.is_empty() && !model.family.is_empty(), "{tag}");
    }
}

#[test]
fn thinking_levels_are_a_filtered_ladder() {
    for (provider, model) in load().models() {
        let tag = format!("{}/{}", provider.id, model.id);
        assert!(!model.thinking_levels.is_empty(), "{tag}: no levels");
        if !model.capabilities.reasoning {
            assert_eq!(
                model.thinking_levels,
                vec![ThinkingLevel::Off],
                "{tag}: non-reasoning models offer only off"
            );
        } else {
            // A reasoning model may still expose a single rung (Grok 4 always reasons and
            // takes no effort parameter) — it just cannot be `off`.
            assert!(
                model.thinking_levels != vec![ThinkingLevel::Off],
                "{tag}: a reasoning model cannot offer only off"
            );
        }
        let ranks: Vec<u8> = model
            .thinking_levels
            .iter()
            .map(|l| level_rank(*l))
            .collect();
        assert!(
            ranks.windows(2).all(|w| w[0] < w[1]),
            "{tag}: levels must be ascending and unique"
        );
    }
}

#[test]
fn clamping_snaps_onto_the_models_ladder() {
    let opus = resolve_ref(&ref_for("anthropic", "claude-opus-5")).unwrap();
    assert_eq!(
        opus.clamp_thinking_level(ThinkingLevel::Xhigh),
        ThinkingLevel::High,
        "xhigh is not offered; the next rung down is"
    );
    assert_eq!(
        opus.clamp_thinking_level(ThinkingLevel::Max),
        ThinkingLevel::Max
    );

    // Always-reasoning model: nothing below `medium` exists, so `off` snaps up.
    let reasoner = resolve_ref(&ref_for("deepseek", "deepseek-reasoner")).unwrap();
    assert_eq!(
        reasoner.clamp_thinking_level(ThinkingLevel::Off),
        ThinkingLevel::Medium
    );
    assert_eq!(reasoner.default_thinking_level(), ThinkingLevel::Medium);

    let legacy = resolve_ref(&ref_for("anthropic", "claude-3-5-haiku")).unwrap();
    assert_eq!(
        legacy.clamp_thinking_level(ThinkingLevel::Max),
        ThinkingLevel::Off
    );
}

#[test]
fn parse_ref_handles_slashes_colons_and_effort() {
    let plain = parse_ref("anthropic/claude-opus-5").unwrap();
    assert_eq!(plain.model_id, "claude-opus-5");
    assert_eq!(plain.thinking_level, ThinkingLevel::Medium);

    let with_effort = parse_ref("anthropic/claude-opus-5:max").unwrap();
    assert_eq!(with_effort.thinking_level, ThinkingLevel::Max);

    // Unsupported effort snaps onto the model's ladder rather than failing.
    assert_eq!(
        parse_ref("anthropic/claude-opus-5:xhigh")
            .unwrap()
            .thinking_level,
        ThinkingLevel::High
    );

    // A model id may contain both separators.
    let proxied = parse_ref("openrouter/anthropic/claude-sonnet-4.5").unwrap();
    assert_eq!(proxied.model_id, "anthropic/claude-sonnet-4.5");
    let tagged = parse_ref("ollama/qwen3:32b").unwrap();
    assert_eq!(tagged.model_id, "qwen3:32b");

    // Unknown model under a known provider is allowed — local lists are open.
    let local = parse_ref("ollama/mystery-model:7b").unwrap();
    assert_eq!(local.model_id, "mystery-model:7b");
    assert_eq!(local.thinking_level, ThinkingLevel::Off);

    assert!(parse_ref("nope/whatever").is_none());
    assert!(parse_ref("claude-opus-5").is_none());
    assert!(parse_ref("anthropic/").is_none());
    assert!(parse_ref("").is_none());
}

#[test]
fn format_ref_round_trips() {
    // Pinned to a specific ref rather than whatever the default happens to be: formatting is
    // what is under test, and it should not change when the shipped default model changes.
    let original = ref_for("anthropic", "claude-opus-5");
    let formatted = format_ref(&original);
    assert_eq!(formatted, "anthropic/claude-opus-5:off");
    assert_eq!(parse_ref(&formatted).unwrap(), original);

    // A model id containing a slash still round-trips, which is the OpenRouter shape.
    let free = ref_for("openrouter", "nvidia/nemotron-3-super-120b-a12b:free");
    assert_eq!(parse_ref(&format_ref(&free)).unwrap(), free);
}

#[test]
fn default_ref_resolves() {
    // The name is deliberately not asserted: the default is a product decision that moves.
    // What must hold is that the shipped default resolves in the offline fallback catalog and
    // offers the effort it ships with, so a first launch with no network still has a model.
    let model = resolve(&default_ref()).expect("default model must exist");
    assert!(model
        .thinking_levels
        .contains(&default_ref().thinking_level));
    assert_eq!(default_ref(), crate::app::default_model_ref());
}

#[test]
fn search_ranks_exact_prefix_first() {
    let hits = search("sonnet");
    assert!(!hits.is_empty());
    // "Sonnet 4.5" starts with the query; "Claude Sonnet 4.5" only contains it.
    assert_eq!(hits[0].provider_id, "anthropic");
    assert_eq!(hits[0].model_id, "claude-sonnet-4-5");
    assert!(hits[0].score > hits[1].score);

    let exact = search("claude-opus-5");
    assert_eq!(exact[0].model_id, "claude-opus-5");
    assert!(exact[0].score >= 1000.0);

    let by_ref = search("anthropic/claude-haiku-4-5");
    assert_eq!(by_ref[0].model_id, "claude-haiku-4-5");

    // Scores are monotonically non-increasing.
    assert!(search("gpt").windows(2).all(|w| w[0].score >= w[1].score));
}

#[test]
fn search_matches_provider_family_and_subsequence() {
    let by_provider = search("groq");
    assert!(!by_provider.is_empty());
    assert!(by_provider.iter().all(|h| h.provider_id == "groq"));

    let by_family = search("gemini");
    assert!(by_family
        .iter()
        .any(|h| h.provider_id == "google" && h.model_id == "gemini-3-pro"));

    // "co5" is a subsequence of "claude-opus-5" but of nothing better.
    let fuzzy = search("co5");
    assert!(fuzzy.iter().any(|h| h.model_id == "claude-opus-5"));

    assert!(search("zzzzzz").is_empty());
}

#[test]
fn empty_search_lists_the_whole_catalog_in_order() {
    let hits = search("  ");
    let total = load().models().count();
    assert_eq!(hits.len(), total);
    assert_eq!(hits[0].provider_id, "anthropic");
    assert_eq!(hits[0].model_id, "claude-opus-5");
}

#[test]
fn deprecated_models_sort_below_live_ones() {
    let hits = search("haiku");
    let live = hits.iter().position(|h| h.model_id == "claude-haiku-4-5");
    let old = hits.iter().position(|h| h.model_id == "claude-3-5-haiku");
    assert!(live < old, "deprecated Haiku 3.5 must not lead");
}

#[test]
fn price_is_per_million_tokens() {
    // Pinned to a paid model: the default is a free one, and zero times anything is zero.
    let opus = resolve(&ref_for("anthropic", "claude-opus-5")).unwrap();
    let usage = Usage {
        input: 1_000_000,
        output: 100_000,
        cache_read: 2_000_000,
        cache_write: 400_000,
        cache_write_1h: Some(100_000),
        total_tokens: 3_600_000,
        ..Default::default()
    };
    let cost = price(&opus, &usage);
    assert!((cost.input - 5.0).abs() < 1e-9);
    assert!((cost.output - 2.5).abs() < 1e-9);
    assert!((cost.cache_read - 1.0).abs() < 1e-9);
    // 0.4M at 6.25 + 0.1M at 10.0 (the 1h tier is 1.6x the 5m write).
    assert!((cost.cache_write - (2.5 + 1.0)).abs() < 1e-9);
    assert!((cost.total - (5.0 + 2.5 + 1.0 + 3.5)).abs() < 1e-9);

    // Local models are free.
    let local = resolve(&ref_for("ollama", "qwen3:32b")).unwrap();
    assert_eq!(price(&local, &usage).total, 0.0);
}
