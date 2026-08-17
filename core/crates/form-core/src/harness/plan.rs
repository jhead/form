//! Turn planning.
//!
//! The whole shape of a run — how many turns, what each one says, which tools it calls, how
//! fast it streams, whether it fails — is decided up front from the seed, so a run is a pure
//! function of `(session_id, turn_index)` and nothing about it depends on wall-clock timing.

use rand::Rng;

use super::content::{self, Response, Shape};
use super::rng::{pick, rng};
use super::tools::{plan_tool, PlannedTool};

/// Cap on turns per run. Real agents stop when they run out of tool calls; this bounds the
/// pathological case where queued prompts keep extending the run.
pub(super) const MAX_TURNS: u32 = 8;

pub(super) struct TurnPlan {
    /// Time to first token, before the speed multiplier.
    pub ttft_ms: u64,
    /// Milliseconds between text deltas, before the multiplier.
    pub delta_ms: u64,
    /// Words per text delta.
    pub words_per_delta: usize,
    pub thinking: Option<String>,
    pub response: Response,
    pub tools: Vec<PlannedTool>,
    /// The turn ends `error { reason: error }` and the run ends `failed` (spec 02 §3).
    pub failure: Option<&'static str>,
}

/// One provider-shaped failure in roughly one run in nine, drawn from the classes the UI has
/// to render differently.
const FAILURES: &[&str] = &[
    "overloaded_error: the model is temporarily overloaded, retry in a moment",
    "rate_limit_error: request rate exceeded for claude-opus-5, retry after 14s",
    "api_error: upstream returned 500 while streaming",
    "context_length_exceeded: the transcript no longer fits the context window",
];

/// Model tiers only differ in cadence — a bigger model thinks longer and streams slower.
#[derive(Clone, Copy)]
enum Tier {
    Fast,
    Mid,
    Deep,
}

fn tier_of(model_id: &str) -> Tier {
    let id = model_id.to_ascii_lowercase();
    if id.contains("opus") || id.contains("max") || id.contains("pro") {
        Tier::Deep
    } else if id.contains("haiku") || id.contains("mini") || id.contains("flash") {
        Tier::Fast
    } else {
        Tier::Mid
    }
}

/// The turns of one run, starting at `turn_index`. Non-final turns always carry tool calls —
/// that is what makes another turn happen, exactly as in `pi`'s agent loop.
pub(super) fn plan_run(
    session_id: &str,
    turn_index: u32,
    model_id: &str,
    workspace_root: Option<&str>,
    prompt: &str,
    thinking_enabled: bool,
) -> Vec<TurnPlan> {
    let tier = tier_of(model_id);
    // The prompt is part of the key as well as the session and the turn. Spec 02 §5 asks for
    // `(session_id, turn_index)`; including the prompt keeps that guarantee — a replay of the
    // same conversation reproduces byte for byte — while stopping two different questions in
    // one session from getting the same answer, which is what happens today because `Core`
    // always passes `turn_index: 0`. See the report: the session's turn count belongs there.
    let key = format!("{session_id}\u{1}{prompt}");
    let mut head = rng(&key, turn_index);
    let turns = head.gen_range(1..=3u32);
    let failing_turn = if head.gen_ratio(1, 9) {
        head.gen_range(0..turns)
    } else {
        u32::MAX
    };

    (0..turns)
        .map(|i| {
            // Seeded per turn rather than per run, so a run that is extended by a queued
            // prompt still reproduces its earlier turns byte for byte.
            let mut r = rng(&key, turn_index + i);
            let last = i + 1 == turns;
            let failure = (i == failing_turn).then(|| *pick(&mut r, FAILURES));

            let shape = if failure.is_some() {
                Shape::Brief
            } else if !last {
                Shape::Handoff
            } else {
                *pick(
                    &mut r,
                    &[
                        Shape::Standard,
                        Shape::Standard,
                        Shape::Long,
                        Shape::Truncated,
                    ],
                )
            };

            let response = content::response(&mut r, shape, prompt);
            let thinking = if thinking_enabled && r.gen_ratio(2, 3) {
                Some(content::thinking(&mut r, prompt))
            } else {
                None
            };

            // A truncated response was cut off mid-arguments, so its tool calls are unsafe to
            // run — `pi` fails the whole batch. Simpler and equivalent here: emit none.
            let tool_count = if last || failure.is_some() || response.truncated {
                0
            } else {
                r.gen_range(1..=5)
            };
            let tools = (0..tool_count)
                .map(|n| plan_tool(&mut r, workspace_root, n))
                .collect();

            let (ttft, delta) = match tier {
                Tier::Fast => (r.gen_range(300..700), r.gen_range(18..30)),
                Tier::Mid => (r.gen_range(400..950), r.gen_range(22..36)),
                Tier::Deep => (r.gen_range(600..1_200), r.gen_range(28..45)),
            };

            // Providers batch harder on long outputs, and a 400-line answer emitted six words
            // at a time would take minutes of wall clock at speed 1.0.
            let batch = if shape == Shape::Long { 3 } else { 1 };

            TurnPlan {
                ttft_ms: ttft,
                delta_ms: delta,
                words_per_delta: r.gen_range(3..=8) * batch,
                thinking,
                response,
                tools,
                failure,
            }
        })
        .collect()
}
