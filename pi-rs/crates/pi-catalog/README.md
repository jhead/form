# pi-catalog

The generated model catalog and the provider registry.

Port of `packages/ai/src/{models,models-store,model-catalog}.ts` and
`packages/ai/src/providers/*.ts`.

| | |
|---|---|
| Embedded models | **1283** across **39** provider shards |
| Built-in providers | **40** (39 with static catalogs + `radius`, which is purely dynamic) |
| Data schema version | 3 |
| Generated at | 2026-08-17T01:17:41Z |

## What this crate does

- **`model_catalog`** — the generated models, embedded from `data/*.json` with
  `include_str!` and parsed once behind a `OnceLock`.
- **`providers`** — the built-in `Provider` descriptors: id, name, base URL,
  auth metadata, model list, and the api ids each provider's models use.
- **`registry`** — `ModelRegistry`: provider registration, model lookup and
  listing, filtering, `provider/model` reference resolution, and runtime
  registration of `Arc<dyn ApiClient>` adapters.
- **`models_store`** — persisted catalogs for dynamic providers.
- **`models`** — thinking-level and cost helpers for a single model.

## Registering adapters

`pi-catalog` has **no dependency on the `pi-provider-*` crates** — the
dependency runs the other way. A provider descriptor only names the api ids its
models use; the adapters are supplied at runtime, keyed by api id:

```rust
use std::sync::Arc;
use pi_catalog::ModelRegistry;

let registry = ModelRegistry::with_builtins();
registry.register_api(Arc::new(AnthropicMessagesApi::new(http)));  // keyed by ApiClient::api()
registry.register_api(Arc::new(OpenAiResponsesApi::new(http)));

let model = registry.find_model("anthropic/claude-sonnet-4-5").unwrap();
let client = registry.client_for_model(&model)?;
```

`register_api_as(api_id, client)` binds an adapter to an explicit id, which is
how an OpenAI-compatible gateway gets served under a custom api id.

Custom providers and models are added the same way at runtime, via
`set_provider`, `set_model` and `set_provider_models`.

`ModelRegistry` is internally synchronized: every method takes `&self`, so it
can be shared as `Arc<ModelRegistry>` and mutated from any thread.

## Refreshing the model data

The per-provider catalogs are **not** hand-maintained and must not be edited by
hand. Upstream generates them with `packages/ai/scripts/generate-models.ts`,
which fetches `https://models.dev/api.json` and then applies roughly 2000 lines
of provider-specific corrections — pricing fixes, reasoning-effort maps,
`compat` flags, Bedrock inference-profile ids, model exclusions. That script is
deliberately **not** ported; we run it and vendor its output.

To refresh (needs network and Node 22+):

```sh
cd .upstream
npm install --ignore-scripts                    # once; node_modules is gitignored
npm --prefix packages/ai run generate-models    # writes packages/ai/src/providers/data/

cp packages/ai/src/providers/data/*.json        ../crates/pi-catalog/data/
cp packages/ai/src/providers/data/.manifest.json ../crates/pi-catalog/data/manifest.json

cd .. && cargo test -p pi-catalog
```

Notes:

- The upstream manifest is a dotfile (`.manifest.json`); it is vendored as
  `manifest.json` so `include_str!` and `cargo package` treat it normally.
- Adding or removing a provider shard also means editing the `PROVIDER_DATA`
  table in `src/model_catalog.rs` and the descriptor table in `src/providers.rs`
  — both are explicit so the change shows up in review.
- `tests/model_data.rs` is the safety net. It asserts every entry deserializes
  into `pi_core::Model`, that each model matches the shard it ships in, that ids
  are unique per provider, that provider descriptors declare every api their
  models use, and that no compat key silently fails to bind (see below).
  Some tests pin specific models and prices ported from upstream's suite; a
  refresh that legitimately changes those needs the assertions updated too.

## Compat wire-shape guard

All 25 compat keys the generated catalog uses bind to real `pi_core::ModelCompat`
fields; nothing falls through to `ModelCompat::extra`. The
`compat_wire_keys_all_bind_to_fields` test enforces that.

Watch out for initialisms when adding fields. `rename_all = "camelCase"` turns
`supports_openai_grammar_tools` into `supportsOpenaiGrammarTools`, which does not
match upstream's `supportsOpenAIGrammarTools` — that mismatch silently dropped
the flag on ~100 models until `pi-core` added an explicit
`#[serde(rename = "supportsOpenAIGrammarTools")]`. New keys with embedded
initialisms need the same treatment.
