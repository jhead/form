//! Usage cost accounting. Port of `calculateCost` in `packages/ai/src/models.ts`.
//!
//! There were four byte-equivalent copies of this (anthropic, openai, google,
//! misc) plus one in `pi-catalog`. The adapters cannot see `pi-catalog` — that
//! arrow points the other way, so the catalog can register adapters without
//! depending on them — which is why the shared copy lands here rather than
//! there.
//!
//! The thinking-level helpers that used to sit alongside this
//! (`getSupportedThinkingLevels` / `clampThinkingLevel`) are now
//! [`pi_core::Model::supported_thinking_levels`] and
//! [`pi_core::Model::clamp_thinking_level`]; they read nothing but a `Model`.

use pi_core::message::Usage;
use pi_core::model::Model;

/// Recompute `usage.cost` in place from the model's (possibly tiered) rates.
///
/// Rates are quoted per million tokens.
pub fn calculate_cost(model: &Model, usage: &mut Usage) {
    // Tier thresholds are unsigned; usage counters are signed so corrective
    // records can carry negative deltas. A negative total therefore matches no
    // tier rather than wrapping into a huge positive.
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let rates = model.rates_for(input_tokens).clone();

    // Anthropic charges 2x base input for 1h cache writes. Upstream subtracts
    // plainly and lets `short_write` go negative for corrective records, so this
    // must not saturate. (`pi-catalog`'s copy uses `saturating_sub` and so
    // differs here; see the report accompanying this consolidation.)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::model::{Api, ModelCost, ModelCostRates, ModelCostTier, ModelThinkingLevel};

    fn model() -> Model {
        let mut model = Model::new("m", Api::MistralConversations, "mistral", "https://x");
        model.cost = ModelCost {
            rates: ModelCostRates {
                input: 1.0,
                output: 2.0,
                cache_read: 0.5,
                cache_write: 4.0,
            },
            tiers: None,
        };
        model
    }

    #[test]
    fn computes_cost_per_million_tokens() {
        let mut usage = Usage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 100_000,
            ..Default::default()
        };
        calculate_cost(&model(), &mut usage);
        assert_eq!(usage.cost.input, 1.0);
        assert_eq!(usage.cost.output, 1.0);
        assert_eq!(usage.cost.cache_read, 1.0);
        assert_eq!(usage.cost.cache_write, 0.4);
        assert_eq!(usage.cost.total, 3.4);
    }

    #[test]
    fn charges_long_cache_writes_at_double_input() {
        let mut usage = Usage {
            cache_write: 1_000_000,
            cache_write_1h: Some(1_000_000),
            ..Default::default()
        };
        calculate_cost(&model(), &mut usage);
        assert_eq!(usage.cost.cache_write, 2.0);
    }

    /// The Anthropic copy's case: a *mix* of 1h and 5m writes, priced apart.
    #[test]
    fn prices_one_hour_writes_at_twice_input_alongside_short_writes() {
        let mut model = Model::new(
            "claude-opus-4-8",
            Api::AnthropicMessages,
            "anthropic",
            "https://api.anthropic.com",
        );
        model.cost = ModelCost {
            rates: ModelCostRates {
                input: 5.0,
                output: 25.0,
                cache_read: 0.5,
                cache_write: 6.25,
            },
            tiers: None,
        };
        let mut usage = Usage {
            cache_write: 1_000_000,
            cache_write_1h: Some(400_000),
            ..Default::default()
        };
        calculate_cost(&model, &mut usage);
        // 6.25 * 0.6 + 5.0 * 2 * 0.4 = 3.75 + 4.0
        assert!((usage.cost.cache_write - 7.75).abs() < 1e-10);
    }

    #[test]
    fn applies_pricing_tiers() {
        let mut model = model();
        model.cost.tiers = Some(vec![ModelCostTier {
            rates: ModelCostRates {
                input: 10.0,
                output: 20.0,
                cache_read: 5.0,
                cache_write: 40.0,
            },
            input_tokens_above: 200_000,
        }]);
        let mut usage = Usage {
            input: 1_000_000,
            ..Default::default()
        };
        calculate_cost(&model, &mut usage);
        assert_eq!(usage.cost.input, 10.0);
    }

    /// Kept from the misc copy: the adapters lean on `Model::clamp_thinking_level`
    /// now, and this pins the behaviour they used to get from a local copy.
    #[test]
    fn clamps_to_supported_levels() {
        let mut model = model();
        model.reasoning = false;
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::High),
            ModelThinkingLevel::Off
        );

        model.reasoning = true;
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::High),
            ModelThinkingLevel::High
        );
        // xhigh/max need an explicit mapping.
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::Max),
            ModelThinkingLevel::High
        );
    }

    /// From the openai copy: an explicitly-null level is skipped *upwards*.
    #[test]
    fn clamps_an_unsupported_level_upwards() {
        let mut model = model();
        model.reasoning = true;
        let mut map = std::collections::BTreeMap::new();
        map.insert(ModelThinkingLevel::Minimal, None);
        map.insert(ModelThinkingLevel::Low, None);
        model.thinking_level_map = Some(map);
        assert_eq!(
            model.clamp_thinking_level(ModelThinkingLevel::Low),
            ModelThinkingLevel::Medium
        );
    }
}
