//! Model-level helper functions.
//!
//! Port of the free functions in `packages/ai/src/models.ts`
//! (`getSupportedThinkingLevels`, `clampThinkingLevel`, `calculateCost`,
//! `modelsAreEqual`). The `Models` collection itself is [`crate::registry`],
//! because in this port auth resolution lives in `pi-auth` rather than here.

use pi_core::{Cost, Model, ModelCostRates, ModelThinkingLevel, Usage};

/// Thinking levels in ascending order, matching upstream's
/// `EXTENDED_THINKING_LEVELS`. Re-exported from `pi-core`, which owns the
/// canonical order because the level walk lives there.
pub use pi_core::EXTENDED_THINKING_LEVELS;

/// Levels this model actually accepts.
///
/// Thin alias for [`Model::supported_thinking_levels`], which is the canonical
/// implementation — it reads nothing but the model, so it belongs in `pi-core`
/// where every crate can reach it. The name is kept because it is upstream's
/// (`getSupportedThinkingLevels`) and callers import it from here.
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    model.supported_thinking_levels()
}

/// Nearest supported level to `level`: search upward first, then downward,
/// falling back to the model's lowest supported level.
///
/// Alias for [`Model::clamp_thinking_level`]; see the note above.
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    model.clamp_thinking_level(level)
}

/// Whether the model supports any thinking at all beyond `off`.
pub fn supports_thinking(model: &Model) -> bool {
    get_supported_thinking_levels(model)
        .iter()
        .any(|level| *level != ModelThinkingLevel::Off)
}

/// Cost rates that apply to a request with this much total input usage.
///
/// Tiers are request-wide: the highest threshold the usage exceeds wins.
pub fn rates_for_usage(model: &Model, usage: &Usage) -> ModelCostRates {
    // Delegates rather than reimplementing the tier walk: there were two copies
    // of this logic and they were already drifting on how ties resolve.
    model
        .rates_for(usage.input + usage.cache_read + usage.cache_write)
        .clone()
}

/// Fill in `usage.cost` for a completed request. Port of `calculateCost`.
///
/// Rates are per million tokens. Anthropic bills 1h cache writes at 2x the base
/// input rate, which is why `cache_write_1h` is split out of `cache_write`.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> Cost {
    let rates = rates_for_usage(model, usage);

    // Upstream subtracts plainly and lets this go negative for corrective
    // records; `saturating_sub` would clamp at zero and diverge from it.
    let long_write = usage.cache_write_1h.unwrap_or(0);
    let short_write = usage.cache_write - long_write;

    usage.cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write = (rates.cache_write * short_write as f64
        + rates.input * 2.0 * long_write as f64)
        / 1_000_000.0;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost.clone()
}

/// Two models are the same when both id and provider match.
pub fn models_are_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}
