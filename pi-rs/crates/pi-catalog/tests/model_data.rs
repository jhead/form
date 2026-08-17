//! Integrity of the embedded generated catalog.
//!
//! Ports the spirit of `test/model-data-validation.test.ts` and
//! `test/model-catalog-types.test.ts`: upstream validates the generated data at
//! build time against a manifest, and asserts model/api/provider identity per
//! shard. Here the data is embedded, so the equivalent guarantees are checked
//! at test time — every entry must deserialize into `pi_core::Model`, and each
//! model's self-reported provider must match the shard it came from.
//!
//! No network access: the embedded data is the fixture.

use std::collections::HashSet;

use pi_catalog::{
    all_builtin_models, builtin_model, builtin_model_count, builtin_model_data_generated_at,
    builtin_model_data_schema_version, builtin_models, builtin_provider_ids, builtin_providers,
};
use pi_core::{Api, Modality};

/// Loading the catalog at all proves every entry deserializes into `Model`:
/// `model_catalog::load` panics on the first malformed shard.
#[test]
fn every_embedded_entry_deserializes_into_a_model() {
    let models = all_builtin_models();
    assert_eq!(models.len(), builtin_model_count());
    assert!(
        models.len() > 1_000,
        "expected the full generated catalog, got {}",
        models.len()
    );

    for model in &models {
        assert!(!model.id.is_empty(), "model with empty id");
        assert!(!model.name.is_empty(), "{} has no name", model.id);
        assert!(!model.provider.is_empty(), "{} has no provider", model.id);
        assert!(
            !matches!(&model.api, Api::Custom(s) if s.is_empty()),
            "{} has an empty api",
            model.id
        );
        assert!(
            model.context_window > 0,
            "{}/{} has a zero context window",
            model.provider,
            model.id
        );
        assert!(
            model.max_tokens > 0,
            "{}/{} has zero max tokens",
            model.provider,
            model.id
        );
        assert!(
            !model.input.is_empty(),
            "{}/{} declares no input modalities",
            model.provider,
            model.id
        );
    }
}

/// Upstream's `validateModelDataDirectory` rejects a model whose `provider` or
/// `id` disagrees with its shard and key.
#[test]
fn every_model_reports_the_provider_shard_it_ships_in() {
    for provider in builtin_provider_ids() {
        let models = builtin_models(&provider);
        assert!(!models.is_empty(), "{provider} shard is empty");
        for model in models {
            assert_eq!(
                model.provider, provider,
                "{} is filed under {provider}",
                model.id
            );
        }
    }
}

/// Upstream rejects "duplicate model IDs across API groups"; after flattening
/// that becomes a duplicate id within one provider.
#[test]
fn model_ids_are_unique_within_a_provider() {
    for provider in builtin_provider_ids() {
        let mut seen = HashSet::new();
        for model in builtin_models(&provider) {
            assert!(
                seen.insert(model.id.clone()),
                "{provider} lists {} more than once",
                model.id
            );
        }
    }
}

/// The provider descriptors must declare every api their models actually use,
/// otherwise `ModelRegistry::client_for_model` could never find an adapter.
#[test]
fn provider_descriptors_declare_every_api_their_models_use() {
    for provider in builtin_providers() {
        for model in &provider.models {
            assert!(
                provider.supports_api(&model.api),
                "provider {} does not declare api {} used by {}",
                provider.id,
                model.api,
                model.id
            );
        }
    }
}

#[test]
fn known_anthropic_model_has_the_right_api_provider_and_window() {
    let model = builtin_model("anthropic", "claude-sonnet-4-5").expect("claude-sonnet-4-5");
    assert_eq!(model.api, Api::AnthropicMessages);
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.base_url, "https://api.anthropic.com");
    assert_eq!(model.context_window, 1_000_000);
    assert_eq!(model.max_tokens, 64_000);
    assert!(model.reasoning);
    assert!(model.supports_images());
    assert_eq!(model.cost.rates.input, 3.0);
    assert_eq!(model.cost.rates.output, 15.0);
    assert_eq!(model.cost.rates.cache_read, 0.3);
    assert_eq!(model.cost.rates.cache_write, 3.75);
    assert_eq!(
        model.compat.as_ref().and_then(|c| c.supports_strict_tools),
        Some(true)
    );
}

#[test]
fn known_openai_model_has_the_right_api_provider_and_window() {
    let model = builtin_model("openai", "gpt-4o").expect("gpt-4o");
    assert_eq!(model.api, Api::OpenAiResponses);
    assert_eq!(model.provider, "openai");
    assert_eq!(model.base_url, "https://api.openai.com/v1");
    assert_eq!(model.context_window, 128_000);
    assert_eq!(model.max_tokens, 16_384);
    assert!(!model.reasoning);
    assert_eq!(model.input, vec![Modality::Text, Modality::Image]);
}

#[test]
fn known_google_model_has_the_right_api_provider_and_window() {
    let model = builtin_model("google", "gemini-2.5-pro").expect("gemini-2.5-pro");
    assert_eq!(model.api, Api::GoogleGenerativeAi);
    assert_eq!(model.provider, "google");
    assert_eq!(
        model.base_url,
        "https://generativelanguage.googleapis.com/v1beta"
    );
    assert_eq!(model.context_window, 1_048_576);
    assert_eq!(model.max_tokens, 65_536);
    assert!(model.reasoning);
}

/// `providers.test.ts`: "stores native constrained-sampling capabilities in
/// model metadata".
#[test]
fn constrained_sampling_capabilities_are_stored_in_model_metadata() {
    let compat = builtin_model("openai", "gpt-4o")
        .expect("gpt-4o")
        .compat
        .expect("gpt-4o compat");
    assert_eq!(compat.supports_strict_mode, Some(true));
    assert_eq!(compat.supports_openai_grammar_tools, None);

    let compat = builtin_model("openai", "gpt-5.4")
        .expect("gpt-5.4")
        .compat
        .expect("gpt-5.4 compat");
    assert_eq!(compat.supports_strict_mode, Some(true));
    assert_eq!(compat.supports_openai_grammar_tools, Some(true));

    let haiku = builtin_model("anthropic", "claude-haiku-4-5").expect("claude-haiku-4-5");
    assert_eq!(
        haiku.compat.and_then(|c| c.supports_strict_tools),
        Some(true)
    );
}

/// `providers.test.ts`: request-wide pricing tiers survive the round trip.
#[test]
fn pricing_tiers_are_preserved() {
    let model = builtin_model("openai", "gpt-5.4").expect("gpt-5.4");
    let tiers = model.cost.tiers.expect("gpt-5.4 tiers");
    assert_eq!(tiers.len(), 1);
    assert_eq!(tiers[0].input_tokens_above, 272_000);
    assert_eq!(tiers[0].rates.input, 5.0);
    assert_eq!(tiers[0].rates.output, 22.5);
}

/// `providers.test.ts`: "uses official Kimi K3 pricing for Moonshot providers".
#[test]
fn moonshot_providers_use_official_kimi_k3_pricing() {
    for provider in ["moonshotai", "moonshotai-cn"] {
        let model = builtin_model(provider, "kimi-k3").expect("kimi-k3");
        assert_eq!(model.cost.rates.input, 3.0);
        assert_eq!(model.cost.rates.output, 15.0);
        assert_eq!(model.cost.rates.cache_read, 0.3);
        assert_eq!(model.cost.rates.cache_write, 0.0);
    }
}

/// `providers.test.ts`: "uses API-equivalent implied pricing for Kimi Coding
/// subscription models".
#[test]
fn kimi_coding_uses_api_equivalent_implied_pricing() {
    let k3 = builtin_model("kimi-coding", "k3").expect("k3");
    assert_eq!(k3.cost.rates.input, 3.0);
    assert_eq!(k3.cost.rates.output, 15.0);
    assert_eq!(k3.cost.rates.cache_read, 0.3);

    let highspeed = builtin_model("kimi-coding", "kimi-for-coding-highspeed")
        .expect("kimi-for-coding-highspeed");
    assert_eq!(highspeed.cost.rates.input, 1.9);
    assert_eq!(highspeed.cost.rates.output, 8.0);
    assert_eq!(highspeed.cost.rates.cache_read, 0.38);
}

/// Port of `together-models.test.ts`.
#[test]
fn together_registers_kimi_k26_on_openai_completions() {
    let model = builtin_model("together", "moonshotai/Kimi-K2.6").expect("Kimi-K2.6");
    assert_eq!(model.api, Api::OpenAiCompletions);
    assert_eq!(model.base_url, "https://api.together.ai/v1");
    assert!(model.reasoning);
    assert_eq!(model.context_window, 262_144);
    assert_eq!(model.max_tokens, 131_000);
    assert_eq!(model.input, vec![Modality::Text, Modality::Image]);
    assert_eq!(model.cost.rates.input, 1.2);
    assert_eq!(model.cost.rates.output, 4.5);

    let compat = model.compat.expect("compat");
    assert_eq!(compat.supports_store, Some(false));
    assert_eq!(compat.supports_developer_role, Some(false));
    assert_eq!(compat.supports_reasoning_effort, Some(false));
    assert_eq!(
        compat.max_tokens_field,
        Some(pi_core::MaxTokensField::MaxTokens)
    );
    assert_eq!(
        compat.thinking_format,
        Some(pi_core::ThinkingFormat::Together)
    );
    assert_eq!(compat.supports_strict_mode, Some(false));
    assert_eq!(compat.supports_long_cache_retention, Some(false));
}

/// Port of `baseten-models.test.ts` (the catalog half; the payload half belongs
/// to the openai-completions adapter).
#[test]
fn baseten_registers_glm_52_as_a_reasoning_model() {
    let model = builtin_model("baseten", "zai-org/GLM-5.2").expect("GLM-5.2");
    assert_eq!(model.api, Api::OpenAiCompletions);
    assert_eq!(model.base_url, "https://inference.baseten.co/v1");
    assert!(model.reasoning);
    assert_eq!(model.context_window, 1_048_576);
    assert_eq!(model.max_tokens, 262_144);
    assert_eq!(model.input, vec![Modality::Text]);

    let compat = model.compat.expect("compat");
    assert_eq!(compat.supports_reasoning_effort, Some(true));
    assert_eq!(
        compat.thinking_format,
        Some(pi_core::ThinkingFormat::Baseten)
    );
    assert!(compat.chat_template_args.is_some());
}

/// Port of `openrouter-cache-control-models.test.ts`.
#[test]
fn openrouter_anthropic_latest_models_enable_cache_control() {
    for id in [
        "~anthropic/claude-fable-latest",
        "~anthropic/claude-haiku-latest",
        "~anthropic/claude-opus-latest",
        "~anthropic/claude-sonnet-latest",
    ] {
        let model = builtin_model("openrouter", id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(
            model.compat.and_then(|c| c.cache_control_format).as_deref(),
            Some("anthropic"),
            "{id}"
        );
    }
}

/// Port of `bedrock-models.test.ts` (the offline assertions only — the upstream
/// suite's live requests are gated behind AWS credentials).
#[test]
fn bedrock_exposes_claude_opus_5_through_an_inference_profile_only() {
    let models = builtin_models("amazon-bedrock");
    assert!(!models.is_empty());
    assert!(models
        .iter()
        .any(|m| m.id == "global.anthropic.claude-opus-5"));
    assert!(!models.iter().any(|m| m.id == "anthropic.claude-opus-5"));
}

/// Port of `xiaomi-models.test.ts`.
#[test]
fn xiaomi_token_plans_omit_api_billing_only_models() {
    for id in ["mimo-v2-flash", "mimo-v2-omni"] {
        assert!(builtin_model("xiaomi", id).is_some(), "xiaomi/{id}");
    }
    for provider in [
        "xiaomi-token-plan-cn",
        "xiaomi-token-plan-ams",
        "xiaomi-token-plan-sgp",
    ] {
        let ids: Vec<String> = builtin_models(provider).into_iter().map(|m| m.id).collect();
        assert!(!ids.contains(&"mimo-v2-flash".to_string()), "{provider}");
        assert!(!ids.contains(&"mimo-v2-omni".to_string()), "{provider}");
    }
}

/// Wire-shape guard. `ModelCompat::extra` is the overflow bucket for compat
/// keys serde could not bind to a field, so anything landing there is a
/// mismatch between `pi_core::ModelCompat` and the TypeScript wire shape.
///
/// Every compat key the generated catalog uses must bind to a real field. A
/// failure here means either a new upstream key needs a field in `pi-core`, or
/// an existing field's serialized name has drifted from the wire — the way
/// `supportsOpenAIGrammarTools` once did, where `rename_all = "camelCase"`
/// produced `supportsOpenaiGrammarTools` and silently dropped the flag on ~100
/// models. Initialisms are the recurring trap; they need an explicit
/// `#[serde(rename)]`.
#[test]
fn compat_wire_keys_all_bind_to_fields() {
    let mut unbound: Vec<String> = all_builtin_models()
        .into_iter()
        .filter_map(|model| model.compat)
        .flat_map(|compat| compat.extra.keys().cloned().collect::<Vec<_>>())
        .collect();
    unbound.sort();
    unbound.dedup();

    assert!(
        unbound.is_empty(),
        "compat keys that pi_core::ModelCompat fails to bind (they fell through \
         to `extra` and are invisible to field access): {unbound:?}"
    );
}

/// The key that regressed before: assert it binds, and that the models carrying
/// it are actually reachable through the field.
#[test]
fn openai_grammar_tools_binds_to_its_field() {
    let with_flag: Vec<String> = all_builtin_models()
        .into_iter()
        .filter(|model| {
            model
                .compat
                .as_ref()
                .and_then(|c| c.supports_openai_grammar_tools)
                .unwrap_or(false)
        })
        .map(|model| format!("{}/{}", model.provider, model.id))
        .collect();

    assert!(
        with_flag.len() > 50,
        "expected supportsOpenAIGrammarTools on many models, got {}",
        with_flag.len()
    );
    assert!(with_flag.contains(&"openai/gpt-5.4".to_string()));
}

#[test]
fn manifest_metadata_is_available() {
    assert!(builtin_model_data_schema_version() > 0);
    let generated_at = builtin_model_data_generated_at().expect("generatedAt");
    assert!(
        generated_at.starts_with("20"),
        "unexpected generatedAt {generated_at}"
    );
}
