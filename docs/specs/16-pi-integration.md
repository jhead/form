# Spec 16 — Swapping the stub for `pi-rs`

> Written after `pi-rs` completed its port. Not part of the MVP: the MVP ships the stub
> harness so the UX can be exercised with no LLM backend (PRD §3). This is the plan for the
> next step, written while the reasoning is fresh.

## 1. What is already true

`pi-rs` is fully ported — `pi-core`, `pi-http`, `pi-catalog`, `pi-auth`, all four provider
crates, `pi-telemetry`, `pi-tools`, `pi-session`, `pi-agent`, `pi-session-sqlite`,
`pi-protocol`, `pi-client`/`pi-server`.

**The wire-compatibility claim in PRD §4.3 is verified**, not assumed. `form-core`'s
`tests/pi_compat.rs` serializes every transcript type and streaming-event variant form
produces, reads it back as the `pi-core` type, and fails on any dropped or altered field. It
also checks the reverse direction, since a session written by the real harness has to remain
readable here. `pi-core` is a dev-dependency only; nothing in the shipping library links it.

That test is the thing that makes the swap cheap. Keep it running.

## 2. The swap, in order

**Step 1 — re-export instead of redefine.** Delete `form-core/src/protocol/wire.rs` and
replace it with `pub use pi_core::{...}`, promoting `pi-core` from a dev-dependency to a
real one. `tests/pi_compat.rs` becomes trivially true and should be deleted in the same
commit; W6's protocol fixtures and W7's Swift round-trip test become the guard from then on.

The fields form does not currently model — `responseModel`, `deferred`, `rawStopReason`,
`endTurn`, diagnostic `severity` — arrive for free. Swift's `.unknown` handling means the app
tolerates them before it renders them; add them to the Swift mirror deliberately, not by
reflex.

**Step 2 — implement `Harness` over `pi_agent`.** The trait already has the right shape:

```rust
pub trait Harness: Send + Sync {
    async fn run(&self, req: RunRequest, ctx: Arc<dyn RunContext>, abort: AbortSignal);
}
```

`pi-agent`'s loop emits `AgentEvent`s that map one-to-one onto what `RunContext::emit`
already publishes, because form's event set was derived from `pi`'s. The adapter is a file,
not a refactor. `StubHarness` stays — it is how the UI is tested without a network, and
`CoreConfig` should gain a `harness: "stub" | "pi"` field rather than deleting it.

**Step 3 — replace `form-core::catalog` with `pi-catalog`.** W4's hand-written catalog covers
9 providers and 38 models; `pi-catalog` has 1283. Keep form's `ModelRef`/`ThinkingLevel`
types (the app depends on them) and resolve through `pi-catalog` underneath.

**Step 4 — auth.** `pi-auth` never touches stdin; interactive prompting is
`Arc<dyn AuthInteraction>`, which is exactly right for a GUI host. Swift already owns
Keychain storage and the core only ever sees `hasKey` — that boundary does not move.

**Step 5 — sessions.** Decide deliberately between form's SQLite store and `pi-session` +
`pi-session-sqlite`. They are not the same shape: form's store carries app concerns
(`groups`, `pinned`, `idx` ordering, FTS5 search, attachment records) that `pi-session` has
no reason to know about. The likely answer is to keep form's store as the app layer and use
`pi-session`'s entry log for transcript persistence — but that is a design decision with a
migration attached, not a swap.

## 3. What to watch

- **Real tool execution changes the FFI calculus.** PRD §4.1 chose in-process on the grounds
  that the stub executes nothing dangerous. Once `pi-tools` runs real shell commands, the
  sidecar option in §4.1 stops being theoretical. The `CoreTransport` seam exists for this;
  revisit the decision at that point rather than inheriting it.
- **Timing.** The stub's cadence is tuned to feel real (300–1200 ms TTFT, 18–45 ms deltas).
  Real providers are burstier. UI that looks smooth against the stub should be re-checked
  against a live provider before anyone concludes the streaming path is done.
- **Abort.** Form's `AbortSignal` is a cooperative flag polled between events; `pi-core` uses
  its own `AbortSignal` on request options. The adapter must bridge them, and the 100 ms
  abort-latency test should keep passing across the seam.
- **Cost and usage.** Once real usage arrives, `context::estimate_tokens` stops being the
  source of truth for anything the provider reports. Keep it for pre-flight estimates only.
