//! Logic every provider adapter needs, in one place.
//!
//! Upstream keeps these in `packages/ai/src/api/` next to the adapters that use
//! them, and every adapter imports the same file. The Rust port grew one crate
//! per provider family, so each of them had grown a private copy instead. Four
//! copies of `transformMessages` is four chances to drift, and they had already
//! drifted — see the module docs for what had to be arbitrated.
//!
//! Module names track the upstream file names:
//!
//! | upstream | module |
//! |---|---|
//! | `api/transform-messages.ts` | [`mod@transform_messages`] |
//! | `api/simple-options.ts` | [`simple_options`] |
//! | `api/constrained-sampling.ts` | [`constrained_sampling`] |
//! | `models.ts#calculateCost` | [`cost`] |
//!
//! ## What does *not* live here
//!
//! Transport and string utilities (`json_parse`, `estimate`, `hash`,
//! `sanitize_unicode`, `provider_env`, `validation`) belong to [`pi_http`]; the
//! adapters import them from there directly. In particular [`simple_options`]
//! delegates all token estimation to [`pi_http::estimate`] rather than carrying
//! its own — three of the four adapter copies counted characters in a unit that
//! was not JavaScript's `String.length`, and so mis-sized `max_tokens` for any
//! prompt that was not pure ASCII.
//!
//! Model-level helpers that read nothing but a `Model` (`clamp_thinking_level`,
//! `supported_thinking_levels`) live on [`pi_core::Model`] itself.

pub mod constrained_sampling;
pub mod cost;
pub mod sanitize_unicode;
pub mod simple_options;
pub mod transform_messages;

pub use constrained_sampling::{
    append_grammar_tool_input_json_delta, create_grammar_tool_input_properties, grammar_tool_input,
    json_schema_tool_parameters, make_strict_json_schema, resolve_grammar_constrained_sampling,
    resolve_json_schema_strict_sampling, GrammarConstrainedSampling, GrammarToolInputJsonBuffer,
    GrammarToolInputProperties, UnsupportedStrictJsonSchema,
};
pub use cost::calculate_cost;
pub use sanitize_unicode::sanitize_surrogates;
pub use simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
    clamp_reasoning, default_thinking_budgets, thinking_budget_for, AdjustedThinking,
    MIN_ANSWER_TOKENS,
};
pub use transform_messages::{transform_messages, ToolCallIdNormalizer};
