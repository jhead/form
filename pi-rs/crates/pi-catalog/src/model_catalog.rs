//! The generated per-provider model catalogs, embedded at compile time.
//!
//! Upstream keeps these as `packages/ai/src/providers/data/*.json`, produced by
//! `packages/ai/scripts/generate-models.ts` (models.dev plus ~2k lines of
//! provider-specific corrections) and re-exported through
//! `providers/*.models.ts` + `model-catalog.ts`'s `flattenModelCatalog`. That
//! script is not ported; the JSON it produces is vendored under `data/` and
//! read here. See `README.md` for the refresh procedure.
//!
//! Each file is `{ apiId: { modelId: Model } }`. Flattening drops the api group
//! (it is redundant — every model carries its own `api`) and preserves file
//! order, which is what upstream's `Object.values` iteration yields.

use std::sync::OnceLock;

use indexmap::IndexMap;
use pi_core::Model;
use serde::Deserialize;

/// One provider's generated catalog: the raw JSON text plus its provider id.
///
/// Kept as an explicit table rather than a build script so that the embedded
/// set is greppable and a refresh is a visible diff.
const PROVIDER_DATA: &[(&str, &str)] = &[
    (
        "amazon-bedrock",
        include_str!("../data/amazon-bedrock.json"),
    ),
    ("ant-ling", include_str!("../data/ant-ling.json")),
    ("anthropic", include_str!("../data/anthropic.json")),
    (
        "azure-openai-responses",
        include_str!("../data/azure-openai-responses.json"),
    ),
    ("baseten", include_str!("../data/baseten.json")),
    ("cerebras", include_str!("../data/cerebras.json")),
    (
        "cloudflare-ai-gateway",
        include_str!("../data/cloudflare-ai-gateway.json"),
    ),
    (
        "cloudflare-workers-ai",
        include_str!("../data/cloudflare-workers-ai.json"),
    ),
    ("deepseek", include_str!("../data/deepseek.json")),
    ("fireworks", include_str!("../data/fireworks.json")),
    (
        "github-copilot",
        include_str!("../data/github-copilot.json"),
    ),
    ("google", include_str!("../data/google.json")),
    ("google-vertex", include_str!("../data/google-vertex.json")),
    ("groq", include_str!("../data/groq.json")),
    ("huggingface", include_str!("../data/huggingface.json")),
    ("kimi-coding", include_str!("../data/kimi-coding.json")),
    ("minimax", include_str!("../data/minimax.json")),
    ("minimax-cn", include_str!("../data/minimax-cn.json")),
    ("mistral", include_str!("../data/mistral.json")),
    ("moonshotai", include_str!("../data/moonshotai.json")),
    ("moonshotai-cn", include_str!("../data/moonshotai-cn.json")),
    ("nvidia", include_str!("../data/nvidia.json")),
    ("openai", include_str!("../data/openai.json")),
    ("openai-codex", include_str!("../data/openai-codex.json")),
    ("opencode", include_str!("../data/opencode.json")),
    ("opencode-go", include_str!("../data/opencode-go.json")),
    ("openrouter", include_str!("../data/openrouter.json")),
    (
        "qwen-token-plan",
        include_str!("../data/qwen-token-plan.json"),
    ),
    (
        "qwen-token-plan-cn",
        include_str!("../data/qwen-token-plan-cn.json"),
    ),
    (
        "qwen-token-plan-individual",
        include_str!("../data/qwen-token-plan-individual.json"),
    ),
    ("together", include_str!("../data/together.json")),
    (
        "vercel-ai-gateway",
        include_str!("../data/vercel-ai-gateway.json"),
    ),
    ("xai", include_str!("../data/xai.json")),
    ("xiaomi", include_str!("../data/xiaomi.json")),
    (
        "xiaomi-token-plan-ams",
        include_str!("../data/xiaomi-token-plan-ams.json"),
    ),
    (
        "xiaomi-token-plan-cn",
        include_str!("../data/xiaomi-token-plan-cn.json"),
    ),
    (
        "xiaomi-token-plan-sgp",
        include_str!("../data/xiaomi-token-plan-sgp.json"),
    ),
    ("zai", include_str!("../data/zai.json")),
    ("zai-coding-cn", include_str!("../data/zai-coding-cn.json")),
];

const MANIFEST: &str = include_str!("../data/manifest.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    generated_at: String,
}

pub(crate) struct Catalog {
    /// Provider id -> models, both in generated order.
    pub(crate) by_provider: IndexMap<String, Vec<Model>>,
    pub(crate) generated_at: Option<String>,
    pub(crate) schema_version: u32,
}

fn load() -> Catalog {
    let mut by_provider = IndexMap::with_capacity(PROVIDER_DATA.len());
    for (provider, json) in PROVIDER_DATA {
        // A parse failure here is a build-time data error, not a runtime
        // condition: the JSON ships inside the binary.
        let groups: IndexMap<String, IndexMap<String, Model>> = serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("embedded catalog for {provider} is malformed: {e}"));
        let models: Vec<Model> = groups
            .into_values()
            .flat_map(IndexMap::into_values)
            .collect();
        by_provider.insert((*provider).to_string(), models);
    }

    let manifest: Option<Manifest> = serde_json::from_str(MANIFEST).ok();
    Catalog {
        by_provider,
        generated_at: manifest.as_ref().map(|m| m.generated_at.clone()),
        schema_version: manifest.map(|m| m.schema_version).unwrap_or(0),
    }
}

pub(crate) fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(load)
}

/// Provider ids that ship a generated catalog, in generated order.
///
/// Note this is a subset of [`crate::builtin_providers`]: purely dynamic
/// providers (`radius`) have no static data.
pub fn builtin_provider_ids() -> Vec<String> {
    catalog().by_provider.keys().cloned().collect()
}

/// Every generated model for one provider, in generated order.
pub fn builtin_models(provider: &str) -> Vec<Model> {
    catalog()
        .by_provider
        .get(provider)
        .cloned()
        .unwrap_or_default()
}

/// Every generated model across all providers.
pub fn all_builtin_models() -> Vec<Model> {
    catalog()
        .by_provider
        .values()
        .flat_map(|models| models.iter().cloned())
        .collect()
}

/// One generated model by provider and model id.
pub fn builtin_model(provider: &str, id: &str) -> Option<Model> {
    catalog()
        .by_provider
        .get(provider)?
        .iter()
        .find(|model| model.id == id)
        .cloned()
}

/// Total number of embedded models.
pub fn builtin_model_count() -> usize {
    catalog().by_provider.values().map(Vec::len).sum()
}

/// ISO-8601 timestamp stamped into the data by the upstream generator.
pub fn builtin_model_data_generated_at() -> Option<String> {
    catalog().generated_at.clone()
}

/// Schema version of the embedded data, as recorded by the upstream generator.
pub fn builtin_model_data_schema_version() -> u32 {
    catalog().schema_version
}
