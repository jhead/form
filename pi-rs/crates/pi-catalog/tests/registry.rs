//! Port of the catalog-relevant cases in `test/models-runtime.test.ts` and
//! `test/providers.test.ts`, plus the model-reference resolution ported from
//! `packages/coding-agent/src/core/model-resolver.ts`.
//!
//! Upstream's `Models` object also owns auth resolution and stream dispatch;
//! those cases belong to `pi-auth` (W4) and the provider crates, so they are
//! not ported here. What remains is the registry contract: provider
//! registration and ordering, model listing and lookup, api-adapter binding,
//! filtering, reference resolution and persisted dynamic catalogs.
//!
//! No network access anywhere: the embedded catalog and a fake adapter are the
//! only fixtures.

use std::sync::Arc;

use async_trait::async_trait;
use pi_catalog::{
    calculate_cost, models_are_equal, CatalogError, InMemoryModelsStore, ModelFilter,
    ModelRegistry, ModelsStore, ModelsStoreEntry, Provider,
};
use pi_core::{
    AiError, Api, ApiClient, ApiClientRef, AssistantMessageEventStream, Context, Model, ModelCost,
    ModelCostRates, ModelCostTier, ModelThinkingLevel, SimpleStreamOptions, StreamOptions, Usage,
};

// ---- fixtures ------------------------------------------------------------

/// Minimal adapter. Registry tests only need to observe *which* adapter a model
/// binds to, so the stream methods are inert.
struct FakeApi {
    api: String,
}

impl FakeApi {
    /// Deliberately not `new`: this returns the trait object, not `Self`.
    fn boxed(api: &str) -> ApiClientRef {
        Arc::new(FakeApi {
            api: api.to_string(),
        })
    }
}

#[async_trait]
impl ApiClient for FakeApi {
    fn api(&self) -> &str {
        &self.api
    }

    async fn stream(
        &self,
        _model: &Model,
        _context: &Context,
        _options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        Err(AiError::unsupported("fake adapter does not stream"))
    }

    async fn stream_simple(
        &self,
        _model: &Model,
        _context: &Context,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        Err(AiError::unsupported("fake adapter does not stream"))
    }
}

fn test_model(provider: &str, id: &str) -> Model {
    Model::new(
        id,
        Api::Custom("test-api".to_string()),
        provider,
        "https://example.test/v1",
    )
}

fn model_with_api(provider: &str, id: &str, api: Api) -> Model {
    Model::new(id, api, provider, "https://example.test/v1")
}

/// A provider carrying one `test-api` model, matching upstream's `testProvider`.
fn test_provider(id: &str) -> Provider {
    test_provider_with(id, vec![test_model(id, "model-a")])
}

fn test_provider_with(id: &str, models: Vec<Model>) -> Provider {
    Provider::new(id, Api::Custom("test-api".to_string())).with_models(models)
}

fn approx(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

// ---- provider registration ----------------------------------------------

/// `models-runtime.test.ts`: "registers, replaces, and deletes providers".
#[test]
fn registers_replaces_and_deletes_providers() {
    let registry = ModelRegistry::new();
    registry.set_provider(test_provider("p1"));
    registry.set_provider(test_provider("p2"));
    assert_eq!(registry.provider_ids(), vec!["p1", "p2"]);

    // Replacing keeps the original listing position.
    let replacement = test_provider_with("p1", vec![test_model("p1", "replaced")]);
    registry.set_provider(replacement);
    assert_eq!(registry.provider_ids(), vec!["p1", "p2"]);
    assert_eq!(registry.provider_count(), 2);
    assert_eq!(
        registry.provider_models("p1")[0].id,
        "replaced",
        "replacement did not take effect"
    );

    assert!(registry.delete_provider("p1"));
    assert!(registry.provider("p1").is_none());
    assert!(!registry.delete_provider("p1"), "second delete is a no-op");

    registry.clear_providers();
    assert_eq!(registry.provider_count(), 0);
}

/// `models-runtime.test.ts`: "lists and finds models per provider".
#[test]
fn lists_and_finds_models_per_provider() {
    let registry = ModelRegistry::new();
    registry.set_provider(test_provider_with(
        "p1",
        vec![test_model("p1", "m1"), test_model("p1", "m2")],
    ));
    registry.set_provider(test_provider_with("p2", vec![test_model("p2", "m3")]));

    let ids: Vec<String> = registry.models().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["m1", "m2", "m3"]);

    let p1: Vec<String> = registry
        .provider_models("p1")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(p1, vec!["m1", "m2"]);

    assert!(registry.provider_models("nope").is_empty());
    assert_eq!(
        registry.get_model("p2", "m3").map(|m| m.id),
        Some("m3".into())
    );
    assert!(registry.get_model("p2", "missing").is_none());
    assert_eq!(registry.model_count(), 3);
}

/// Upstream returns `undefined` for both an unknown provider and an unknown
/// model; `require_model` keeps them distinguishable for error reporting.
#[test]
fn require_model_distinguishes_unknown_provider_from_unknown_model() {
    let registry = ModelRegistry::new();
    registry.set_provider(test_provider("p1"));

    assert!(registry.require_model("p1", "model-a").is_ok());
    assert_eq!(
        registry.require_model("p1", "ghost").unwrap_err(),
        CatalogError::UnknownModel {
            provider: "p1".into(),
            model: "ghost".into()
        }
    );
    assert_eq!(
        registry.require_model("ghost", "model-a").unwrap_err(),
        CatalogError::UnknownProvider {
            provider: "ghost".into()
        }
    );
}

// ---- runtime custom models and providers --------------------------------

#[test]
fn custom_providers_and_models_can_be_added_at_runtime() {
    let registry = ModelRegistry::with_builtins();
    let before = registry.model_count();

    let custom = Provider::new("my-gateway", Api::OpenAiCompletions)
        .with_name("My Gateway")
        .with_base_url("https://gateway.internal/v1")
        .with_models(vec![model_with_api(
            "my-gateway",
            "house-model",
            Api::OpenAiCompletions,
        )]);
    registry.set_provider(custom);

    assert_eq!(registry.model_count(), before + 1);
    let resolved = registry
        .find_model("my-gateway/house-model")
        .expect("resolved");
    assert_eq!(resolved.provider, "my-gateway");
    assert_eq!(
        registry.provider("my-gateway").map(|p| p.name),
        Some("My Gateway".to_string())
    );

    // Upsert a second model, then replace it in place.
    let mut extra = model_with_api("my-gateway", "house-model-2", Api::OpenAiCompletions);
    extra.context_window = 32_000;
    registry.set_model(extra).expect("add");
    assert_eq!(registry.provider_models("my-gateway").len(), 2);

    let mut updated = model_with_api("my-gateway", "house-model-2", Api::OpenAiCompletions);
    updated.context_window = 64_000;
    registry.set_model(updated).expect("replace");
    assert_eq!(registry.provider_models("my-gateway").len(), 2);
    assert_eq!(
        registry
            .get_model("my-gateway", "house-model-2")
            .unwrap()
            .context_window,
        64_000
    );

    assert!(registry.remove_model("my-gateway", "house-model-2"));
    assert_eq!(registry.provider_models("my-gateway").len(), 1);
    assert!(!registry.remove_model("my-gateway", "house-model-2"));
}

#[test]
fn adding_a_model_to_an_unknown_provider_is_rejected() {
    let registry = ModelRegistry::new();
    assert_eq!(
        registry.set_model(test_model("ghost", "m")).unwrap_err(),
        CatalogError::UnknownProvider {
            provider: "ghost".into()
        }
    );
}

#[test]
fn a_provider_catalog_can_be_reset_to_the_builtin_data() {
    let registry = ModelRegistry::with_builtins();
    let original = registry.provider_models("anthropic").len();
    assert!(original > 0);

    registry
        .set_provider_models("anthropic", vec![])
        .expect("clear");
    assert_eq!(registry.provider_models("anthropic").len(), 0);

    registry
        .reset_provider_to_builtin("anthropic")
        .expect("reset");
    assert_eq!(registry.provider_models("anthropic").len(), original);
}

// ---- api adapter binding -------------------------------------------------

/// `providers.test.ts`: "dispatches on model.api for mixed-API providers".
#[test]
fn dispatches_on_model_api_for_mixed_api_providers() {
    let registry = ModelRegistry::new();
    let mut mixed = Provider::new("mixed", Api::AnthropicMessages);
    mixed.apis = vec![Api::AnthropicMessages, Api::OpenAiCompletions];
    mixed.models = vec![
        model_with_api("mixed", "model-a", Api::AnthropicMessages),
        model_with_api("mixed", "model-b", Api::OpenAiCompletions),
    ];
    registry.set_provider(mixed);

    registry.register_api(FakeApi::boxed("anthropic-messages"));
    registry.register_api(FakeApi::boxed("openai-completions"));

    let a = registry.get_model("mixed", "model-a").unwrap();
    let b = registry.get_model("mixed", "model-b").unwrap();
    assert_eq!(
        registry.client_for_model(&a).unwrap().api(),
        "anthropic-messages"
    );
    assert_eq!(
        registry.client_for_model(&b).unwrap().api(),
        "openai-completions"
    );
}

/// `providers.test.ts`: "produces a stream error for a model whose api has no
/// implementation".
#[test]
fn a_model_whose_api_has_no_adapter_reports_no_implementation() {
    let registry = ModelRegistry::new();
    let mut provider = Provider::new("mixed", Api::AnthropicMessages);
    provider.models = vec![model_with_api("mixed", "model-a", Api::AnthropicMessages)];
    registry.set_provider(provider);

    let model = registry.get_model("mixed", "model-a").unwrap();
    // `ApiClientRef` is not `Debug`, so `unwrap_err` is unavailable.
    let Err(error) = registry.client_for_model(&model) else {
        panic!("expected a missing-implementation error");
    };
    assert_eq!(
        error,
        CatalogError::NoApiImplementation {
            provider: "mixed".into(),
            api: "anthropic-messages".into()
        }
    );
    assert!(error.to_string().contains("no API implementation"));
    assert_eq!(error.code(), "stream");

    // And it converts into the AiError an error event would carry.
    let ai: AiError = error.into();
    assert_eq!(ai.code(), "unsupported");
}

/// `models-runtime.test.ts`: "produces an error stream for unknown providers
/// instead of throwing".
#[test]
fn an_unknown_provider_is_reported_rather_than_panicking() {
    let registry = ModelRegistry::new();
    let Err(error) = registry.client_for_model(&test_model("ghost", "model-a")) else {
        panic!("expected an unknown-provider error");
    };
    assert_eq!(
        error,
        CatalogError::UnknownProvider {
            provider: "ghost".into()
        }
    );
    assert!(error.to_string().contains("Unknown provider: ghost"));
}

#[test]
fn adapters_are_registered_replaced_and_removed_by_api_id() {
    let registry = ModelRegistry::new();
    assert!(registry.api_ids().is_empty());

    registry.register_api(FakeApi::boxed("anthropic-messages"));
    registry.register_api(FakeApi::boxed("openai-completions"));
    assert_eq!(
        registry.api_ids(),
        vec!["anthropic-messages", "openai-completions"]
    );
    assert!(registry.api_client(&Api::AnthropicMessages).is_some());
    assert!(registry.api_client(&Api::GoogleVertex).is_none());

    // An explicit id lets one adapter serve a custom api.
    registry.register_api_as("my-custom-api", FakeApi::boxed("openai-completions"));
    assert_eq!(
        registry
            .api_client(&Api::Custom("my-custom-api".into()))
            .unwrap()
            .api(),
        "openai-completions"
    );

    assert!(registry.unregister_api("anthropic-messages"));
    assert!(!registry.unregister_api("anthropic-messages"));
    assert!(registry.api_client(&Api::AnthropicMessages).is_none());
}

#[test]
fn a_bound_adapter_is_exposed_as_a_stream_fn() {
    let registry = ModelRegistry::with_builtins();
    registry.register_api(FakeApi::boxed("anthropic-messages"));
    let model = registry.find_model("anthropic/claude-sonnet-4-5").unwrap();
    assert!(registry.stream_fn_for_model(&model).is_ok());

    let unbound = registry.find_model("google/gemini-2.5-pro").unwrap();
    assert!(registry.stream_fn_for_model(&unbound).is_err());
}

// ---- reference resolution ------------------------------------------------

#[test]
fn resolves_a_canonical_provider_slash_model_reference() {
    let registry = ModelRegistry::with_builtins();
    let model = registry
        .resolve_model("anthropic/claude-sonnet-4-5")
        .expect("resolved");
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.id, "claude-sonnet-4-5");
    assert_eq!(model.api, Api::AnthropicMessages);
}

#[test]
fn reference_resolution_is_case_insensitive_and_trims() {
    let registry = ModelRegistry::with_builtins();
    let model = registry
        .resolve_model("  Anthropic/Claude-Sonnet-4-5  ")
        .expect("resolved");
    assert_eq!(model.id, "claude-sonnet-4-5");
}

/// Model ids may themselves contain slashes. The canonical `provider/id` match
/// must win over splitting at the first `/`.
#[test]
fn resolves_model_ids_that_contain_slashes() {
    let registry = ModelRegistry::with_builtins();

    let by_canonical = registry
        .resolve_model("openrouter/anthropic/claude-opus-4.6")
        .expect("canonical");
    assert_eq!(by_canonical.provider, "openrouter");
    assert_eq!(by_canonical.id, "anthropic/claude-opus-4.6");

    // Bare, slash-containing model id with a unique owner.
    let bare = registry
        .resolve_model("~anthropic/claude-opus-latest")
        .expect("bare");
    assert_eq!(bare.provider, "openrouter");

    let together = registry
        .resolve_model("together/moonshotai/Kimi-K2.6")
        .expect("together");
    assert_eq!(together.provider, "together");
    assert_eq!(together.id, "moonshotai/Kimi-K2.6");
}

#[test]
fn resolves_a_bare_model_id_when_exactly_one_provider_offers_it() {
    let registry = ModelRegistry::new();
    registry.set_provider(test_provider_with(
        "p1",
        vec![test_model("p1", "only-here")],
    ));
    registry.set_provider(test_provider_with(
        "p2",
        vec![test_model("p2", "elsewhere")],
    ));

    let model = registry.resolve_model("only-here").expect("resolved");
    assert_eq!(model.provider, "p1");
}

/// Upstream returns `undefined` rather than guessing when a bare id is offered
/// by more than one provider.
#[test]
fn an_ambiguous_bare_model_id_does_not_resolve() {
    let registry = ModelRegistry::new();
    registry.set_provider(test_provider_with("p1", vec![test_model("p1", "shared")]));
    registry.set_provider(test_provider_with("p2", vec![test_model("p2", "shared")]));

    assert!(registry.find_model("shared").is_none());
    let error = registry.resolve_model("shared").unwrap_err();
    assert_eq!(error.code(), "unresolved_model");
    let message = error.to_string();
    assert!(message.contains("ambiguous"), "{message}");
    assert!(
        message.contains("p1/shared") && message.contains("p2/shared"),
        "{message}"
    );

    // Qualifying it disambiguates.
    assert_eq!(registry.resolve_model("p2/shared").unwrap().provider, "p2");
}

/// `kimi-k3` is offered by moonshotai, moonshotai-cn and others, so the bare id
/// must not resolve against the real catalog either.
#[test]
fn an_ambiguous_bare_id_in_the_real_catalog_does_not_resolve() {
    let registry = ModelRegistry::with_builtins();
    assert!(registry.find_model("kimi-k3").is_none());
    assert_eq!(
        registry
            .resolve_model("moonshotai/kimi-k3")
            .unwrap()
            .provider,
        "moonshotai"
    );
}

#[test]
fn unmatched_and_empty_references_are_rejected() {
    let registry = ModelRegistry::with_builtins();
    assert!(registry.find_model("no-such-model-anywhere").is_none());
    assert!(registry.resolve_model("").is_err());
    assert!(registry.resolve_model("   ").is_err());
    assert!(registry.resolve_model("anthropic/").is_err());
    assert!(registry.resolve_model("/claude-sonnet-4-5").is_err());
}

// ---- filtering -----------------------------------------------------------

#[test]
fn filters_by_provider_api_and_reasoning() {
    let registry = ModelRegistry::with_builtins();

    let anthropic = registry.list_models(&ModelFilter::new().provider("anthropic"));
    assert!(!anthropic.is_empty());
    assert!(anthropic.iter().all(|m| m.provider == "anthropic"));

    let google = registry.list_models(&ModelFilter::new().api(Api::GoogleGenerativeAi));
    assert!(!google.is_empty());
    assert!(google.iter().all(|m| m.api == Api::GoogleGenerativeAi));

    let reasoning = registry.list_models(&ModelFilter::new().provider("anthropic").reasoning(true));
    let non_reasoning =
        registry.list_models(&ModelFilter::new().provider("anthropic").reasoning(false));
    assert!(!reasoning.is_empty());
    assert!(reasoning.iter().all(|m| m.reasoning));
    assert!(non_reasoning.iter().all(|m| !m.reasoning));
    assert_eq!(reasoning.len() + non_reasoning.len(), anthropic.len());
}

#[test]
fn filters_by_capability_and_thresholds() {
    let registry = ModelRegistry::with_builtins();

    let vision = registry.list_models(&ModelFilter::new().provider("openai").supports_images(true));
    assert!(!vision.is_empty());
    assert!(vision.iter().all(|m| m.supports_images()));

    let big = registry.list_models(&ModelFilter::new().min_context_window(1_000_000));
    assert!(!big.is_empty());
    assert!(big.iter().all(|m| m.context_window >= 1_000_000));

    // Only models that actually accept `max` thinking.
    let max_thinkers = registry.list_models(
        &ModelFilter::new()
            .provider("anthropic")
            .thinking_level(ModelThinkingLevel::Max),
    );
    assert!(!max_thinkers.is_empty());
    assert!(max_thinkers.iter().any(|m| m.id == "claude-opus-5"));
    assert!(!max_thinkers.iter().any(|m| m.id == "claude-sonnet-4-5"));

    let cheap = registry.list_models(&ModelFilter::new().provider("openai").max_input_cost(1.0));
    assert!(cheap.iter().all(|m| m.cost.rates.input <= 1.0));

    let named = registry.list_models(&ModelFilter::new().provider("anthropic").id_contains("OPUS"));
    assert!(!named.is_empty());
    assert!(named.iter().all(|m| m.id.contains("opus")));
}

/// `runnable_only` narrows the catalog to what this process can actually call —
/// the practical use of runtime adapter registration.
#[test]
fn filters_to_models_with_a_registered_adapter() {
    let registry = ModelRegistry::with_builtins();
    assert!(registry.runnable_models().is_empty());
    assert!(registry.runnable_providers().is_empty());

    registry.register_api(FakeApi::boxed("anthropic-messages"));
    let runnable = registry.runnable_models();
    assert!(!runnable.is_empty());
    assert!(runnable.iter().all(|m| m.api == Api::AnthropicMessages));

    let providers: Vec<String> = registry
        .runnable_providers()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert!(providers.contains(&"anthropic".to_string()));
    assert!(!providers.contains(&"google".to_string()));
}

// ---- built-in catalog ----------------------------------------------------

/// `providers.test.ts`: "builtinModels registers every builtin provider with
/// models".
#[test]
fn builtins_register_every_provider_with_its_models() {
    let registry = ModelRegistry::with_builtins();
    let providers = registry.providers();

    assert_eq!(providers.len(), pi_catalog::builtin_providers().len());
    assert!(registry.provider("anthropic").is_some());

    let anthropic = registry
        .get_model("anthropic", "claude-haiku-4-5")
        .expect("haiku");
    assert_eq!(anthropic.api, Api::AnthropicMessages);

    assert!(
        registry.model_count() > 500,
        "expected the full catalog, got {}",
        registry.model_count()
    );

    // Static providers list models immediately; Radius is purely dynamic.
    for provider in providers {
        let models = registry.provider_models(&provider.id);
        if provider.id == "radius" {
            assert!(models.is_empty(), "radius should ship no static models");
            assert!(provider.dynamic);
        } else {
            assert!(!models.is_empty(), "{} has no models", provider.id);
        }
        assert!(models.iter().all(|m| m.provider == provider.id));
    }
}

#[test]
fn builtin_provider_metadata_matches_upstream() {
    let registry = ModelRegistry::with_builtins();

    let anthropic = registry.provider("anthropic").expect("anthropic");
    assert_eq!(anthropic.name, "Anthropic");
    assert_eq!(
        anthropic.base_url.as_deref(),
        Some("https://api.anthropic.com")
    );
    assert_eq!(anthropic.apis, vec![Api::AnthropicMessages]);
    let api_key = anthropic.auth.api_key.expect("api key auth");
    assert_eq!(
        api_key.env_vars,
        vec![
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY"
        ]
    );
    let oauth = anthropic.auth.oauth.expect("oauth");
    assert!(oauth.is_subscription);

    // OpenAI Codex is OAuth-only.
    let codex = registry.provider("openai-codex").expect("openai-codex");
    assert!(codex.auth.api_key.is_none());
    assert!(codex.auth.oauth.expect("oauth").is_subscription);

    // Mixed-API providers declare every api they serve.
    let copilot = registry.provider("github-copilot").expect("github-copilot");
    assert_eq!(
        copilot.apis,
        vec![
            Api::AnthropicMessages,
            Api::OpenAiCompletions,
            Api::OpenAiResponses
        ]
    );

    // Providers whose endpoint is account-derived carry no base URL.
    assert!(registry
        .provider("amazon-bedrock")
        .unwrap()
        .base_url
        .is_none());
    assert!(registry
        .provider("azure-openai-responses")
        .unwrap()
        .base_url
        .is_none());
}

// ---- cost ----------------------------------------------------------------

/// `models-runtime.test.ts`: "applies request-wide pricing tiers above the
/// configured input threshold".
#[test]
fn applies_request_wide_pricing_tiers_above_the_threshold() {
    let mut model = test_model("openai", "gpt-5.6-sol");
    model.cost = ModelCost {
        rates: ModelCostRates {
            input: 5.0,
            output: 30.0,
            cache_read: 0.5,
            cache_write: 6.25,
        },
        tiers: Some(vec![ModelCostTier {
            rates: ModelCostRates {
                input: 10.0,
                output: 45.0,
                cache_read: 1.0,
                cache_write: 12.5,
            },
            input_tokens_above: 272_000,
        }]),
    };

    let usage = |cache_write: i64| Usage {
        input: 200_000,
        output: 100_000,
        cache_read: 72_000,
        cache_write,
        total_tokens: 372_000 + cache_write,
        ..Default::default()
    };

    // 200000 + 72000 + 0 == 272000, which does not *exceed* the threshold.
    let mut short = usage(0);
    let cost = calculate_cost(&model, &mut short);
    approx(cost.input, 1.0);
    approx(cost.output, 3.0);
    approx(cost.cache_read, 0.036);
    approx(cost.cache_write, 0.0);

    // One more token crosses it and the tier rates apply to the whole request.
    let mut long = usage(1);
    let cost = calculate_cost(&model, &mut long);
    approx(cost.input, 2.0);
    approx(cost.output, 4.5);
    approx(cost.cache_read, 0.072);
    approx(cost.cache_write, 0.0000125);
    approx(cost.total, 2.0 + 4.5 + 0.072 + 0.0000125);
}

/// Anthropic bills 1h cache writes at 2x the base input rate.
#[test]
fn long_cache_writes_are_billed_at_twice_the_input_rate() {
    let mut model = test_model("anthropic", "claude-sonnet-4-5");
    model.cost = ModelCost {
        rates: ModelCostRates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        },
        tiers: None,
    };

    let mut usage = Usage {
        cache_write: 1_000_000,
        cache_write_1h: Some(400_000),
        ..Default::default()
    };
    let cost = calculate_cost(&model, &mut usage);
    // 600k short writes at 3.75 + 400k long writes at 2*3.
    approx(cost.cache_write, (3.75 * 600_000.0 + 6.0 * 400_000.0) / 1e6);
}

#[test]
fn models_are_equal_compares_id_and_provider() {
    let a = test_model("p1", "m1");
    let b = test_model("p1", "m1");
    let other_provider = test_model("p2", "m1");

    assert!(models_are_equal(Some(&a), Some(&b)));
    assert!(!models_are_equal(Some(&a), Some(&other_provider)));
    assert!(!models_are_equal(Some(&a), None));
    assert!(!models_are_equal(None, None));
}

// ---- persisted dynamic catalogs -----------------------------------------

/// `models-runtime.test.ts`: "persists dynamic catalogs and restores them
/// without network access".
#[tokio::test]
async fn persists_dynamic_catalogs_and_restores_them_without_network() {
    let store = InMemoryModelsStore::new();

    // "Online": a dynamic provider publishes a fetched catalog.
    let online = ModelRegistry::new();
    online.set_provider(Provider::new("dynamic", Api::PiMessages));
    assert!(online.provider_models("dynamic").is_empty());
    online
        .publish_provider_models(
            &store,
            "dynamic",
            vec![model_with_api("dynamic", "fetched", Api::PiMessages)],
            Some(1_700_000_000_000),
        )
        .await
        .expect("publish");
    assert!(online.get_model("dynamic", "fetched").is_some());

    // "Offline": a fresh registry restores the same catalog from the store
    // without any fetch.
    let offline = ModelRegistry::new();
    offline.set_provider(Provider::new("dynamic", Api::PiMessages));
    assert!(offline
        .restore_provider_models(&store, "dynamic")
        .await
        .expect("restore"));
    assert!(offline.get_model("dynamic", "fetched").is_some());

    let entry = store.read("dynamic").await.unwrap().expect("entry");
    assert_eq!(entry.models.len(), 1);
    assert_eq!(entry.checked_at, Some(1_700_000_000_000));
}

#[test]
fn restoring_an_unknown_provider_is_rejected() {
    let registry = ModelRegistry::new();
    assert_eq!(
        registry.set_provider_models("ghost", vec![]).unwrap_err(),
        CatalogError::UnknownProvider {
            provider: "ghost".into()
        }
    );
}

#[tokio::test]
async fn restoring_reports_false_when_nothing_is_cached() {
    let store = InMemoryModelsStore::new();
    let registry = ModelRegistry::new();
    registry.set_provider(Provider::new("dynamic", Api::PiMessages));

    assert!(!registry
        .restore_provider_models(&store, "dynamic")
        .await
        .expect("restore"));
}

/// A store entry that leaked another provider's models must not contaminate the
/// registry — upstream filters on `model.provider === input.id` when restoring.
#[tokio::test]
async fn restoring_ignores_models_belonging_to_another_provider() {
    let store = InMemoryModelsStore::new();
    store
        .write(
            "dynamic",
            ModelsStoreEntry {
                models: vec![model_with_api("someone-else", "stray", Api::PiMessages)],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let registry = ModelRegistry::new();
    registry.set_provider(Provider::new("dynamic", Api::PiMessages));
    assert!(!registry
        .restore_provider_models(&store, "dynamic")
        .await
        .expect("restore"));
    assert!(registry.provider_models("dynamic").is_empty());
}

#[tokio::test]
async fn the_in_memory_store_round_trips_and_deletes() {
    let store = InMemoryModelsStore::new();
    assert!(store.read("p").await.unwrap().is_none());

    store
        .write(
            "p",
            ModelsStoreEntry {
                models: vec![test_model("p", "m")],
                etag: Some("\"abc\"".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let entry = store.read("p").await.unwrap().expect("entry");
    assert_eq!(entry.models.len(), 1);
    assert_eq!(entry.etag.as_deref(), Some("\"abc\""));

    store.delete("p").await.unwrap();
    assert!(store.read("p").await.unwrap().is_none());
}

// ---- concurrency ---------------------------------------------------------

/// The registry is shared as `Arc<ModelRegistry>` across threads (and handed to
/// a Swift host as an opaque handle), so `&self` mutation must be safe.
#[test]
fn the_registry_is_shareable_across_threads() {
    let registry = Arc::new(ModelRegistry::with_builtins());
    let mut handles = Vec::new();

    for i in 0..8 {
        let registry = Arc::clone(&registry);
        handles.push(std::thread::spawn(move || {
            let id = format!("runtime-{i}");
            registry.set_provider(
                Provider::new(id.clone(), Api::OpenAiCompletions)
                    .with_models(vec![model_with_api(&id, "m", Api::OpenAiCompletions)]),
            );
            registry.register_api(FakeApi::boxed("openai-completions"));
            assert!(registry.find_model("anthropic/claude-sonnet-4-5").is_some());
            assert!(registry.resolve_model(&format!("{id}/m")).is_ok());
        }));
    }
    for handle in handles {
        handle.join().expect("thread panicked");
    }

    assert_eq!(
        registry.providers().len(),
        pi_catalog::builtin_providers().len() + 8
    );
}
