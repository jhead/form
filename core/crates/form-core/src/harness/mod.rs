//! The stub harness — a deterministic event source shaped exactly like the real one.
//!
//! **Owner: W2** (`docs/specs/02-stub-harness.md`).
//!
//! Nothing here executes anything. It produces the event sequence `pi-agent` produces —
//! same ordering, same accumulation of `partial`, same "failures are values on the stream"
//! rule — driven by seeded mock content. When `pi-rs` lands, [`StubHarness`] is replaced by
//! an adapter over `pi_agent::Agent` behind the [`Harness`] trait and nothing else moves.
//!
//! The ordering contract is checked in `tests.rs` against a grammar written out from spec 00
//! §5.1, because ordering is the thing the Swift side is least able to recover from.

mod content;
mod plan;
mod rng;
mod stub;
mod tools;

#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::app::TurnRecord;
use crate::protocol::{Entry, EntryKind, EventKind, ModelRef};

pub use stub::StubHarness;

/// Cooperative cancellation. A Swift caller cannot drop a Rust future, so aborting is an
/// explicit signal the run polls between events — the same convention as `pi-core`.
#[derive(Clone, Default)]
pub struct AbortSignal(Arc<AtomicBool>);

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct RunRequest {
    pub session_id: String,
    pub run_id: String,
    pub command_id: Option<String>,
    pub prompt: String,
    pub model: ModelRef,
    pub workspace_root: Option<String>,
    /// Index of this run's first turn within the session. Content is seeded from
    /// `(session_id, turn_index)`, so passing the session's real turn count is what keeps a
    /// long session from repeating itself.
    pub turn_index: u32,
}

/// Emitting side of the run. The concrete implementation writes to the store and the
/// event bus; the harness only decides *what* happens and *when*.
///
/// Everything below `speed` has a default so the trait can grow without breaking the
/// implementations that do not need it yet.
pub trait RunContext: Send + Sync {
    fn emit(&self, kind: EventKind);
    fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry>;
    fn replace_entry(&self, entry: &Entry);
    /// Multiplier on all sleeps. 1.0 is human-realistic; tests use 100.0.
    fn speed(&self) -> f64;

    /// Pop a prompt queued while this run was streaming (F1.7). The harness calls this at
    /// every turn boundary and injects the message before the next assistant response, which
    /// is where `pi`'s agent loop takes its steering messages.
    fn take_queued_prompt(&self) -> Option<String> {
        None
    }

    /// Tokens the request spends before the transcript — the resolved system prompt plus the
    /// tool schemas. `None` falls back to `context::system_prompt_tokens` over the session's
    /// workspace root alone; override it to fold in the user's own additions from settings,
    /// so the billed input and the context ring's segments cannot disagree.
    fn prompt_overhead_tokens(&self) -> Option<u64> {
        None
    }

    /// One `turns` row and its `tool_invocations` rows per turn (spec 02 §6), so the stats
    /// engine never has to special-case stub data. Recorded for aborted and failed turns
    /// too — a stopped run is a real datum on the dashboard, not a gap. The default drops
    /// it so a context that only cares about events stays a four-method implementation.
    fn record_turn(&self, _turn: TurnRecord) {}
}

#[async_trait::async_trait]
pub trait Harness: Send + Sync {
    async fn run(&self, req: RunRequest, ctx: Arc<dyn RunContext>, abort: AbortSignal);
}
