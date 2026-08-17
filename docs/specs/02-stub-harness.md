# Spec 02 — Stub harness (`form-core::harness`)

> **Workstream W2.** Owns `core/crates/form-core/src/harness/`. Do not implement a real
> agent, a real provider call, or real tool execution. The point is a *convincing,
> deterministic event source* that emits exactly the protocol `pi-rs` will emit.

## 1. Why this exists

The UI cannot be evaluated against a static fixture — streaming cadence, event ordering,
partial-state reconciliation and abort behaviour are the things most likely to be wrong.
This module produces the real event sequence with fake content, so every UI path is
exercised without an API key.

When `pi-rs` lands, this module is replaced by an adapter over `pi_agent::Agent` behind the
same `Harness` trait. Keep the trait clean enough that the swap is a file, not a refactor.

## 2. Trait

```rust
pub trait Harness: Send + Sync {
    fn run(&self, req: RunRequest, sink: EventSink, abort: AbortSignal) -> RunHandle;
}

pub struct RunRequest {
    pub session_id: String,
    pub run_id: String,
    pub prompt: UserMessage,
    pub transcript: Vec<Entry>,
    pub model: ModelRef,
    pub workspace_root: Option<PathBuf>,
}
```

`EventSink` is the same channel shape as `pi_core::AssistantMessageEventSink` — an async
`send` returning `false` once the consumer is gone.

## 3. Emission contract

Per run, in order:

1. `run_start`
2. For each turn:
   `turn_start`
   → `message_start` (the assistant entry, empty content)
   → a stream of `message_update` wrapping `AssistantMessageEvent`s:
   `start` → optionally `thinking_start`/`thinking_delta`*/`thinking_end`
   → `text_start`/`text_delta`*/`text_end`
   → optionally `toolcall_start`/`toolcall_delta`*/`toolcall_end` (repeatable)
   → `done { reason }`
   → `message_end`
   → for each tool call: `tool_execution_start`, `tool_execution_update`*,
     `tool_execution_end`, then a tool-result `message_start`/`message_end`
   → `turn_end { usage }`
3. `run_end { outcome, usage, durationMs }`

Every non-terminal `AssistantMessageEvent` carries the accumulated `partial:
AssistantMessage`. **The partial must be genuinely accumulated**, not a stub — the Swift
side reconciles against it.

Failures are encoded in the stream (`error { reason }` + `run_end { outcome: "failed" }`),
never returned from `dispatch`. Aborts produce `error { reason: "aborted" }` and
`run_end { outcome: "aborted" }` and must land within 100 ms of the abort command.

## 4. Timing

Realistic cadence — this is what makes the UI feel right:

| Phase | Timing |
|---|---|
| Time to first token | 300–1200 ms, sampled per model tier |
| Text delta | 3–8 tokens per event, 18–45 ms apart, with jitter |
| Thinking delta | same, but ~2× faster and in longer runs |
| Tool call args stream | 4–10 fragments, 30 ms apart (exercises partial-JSON rendering) |
| Tool execution | 200 ms – 4 s, with 3–8 `tool_execution_update` progress ticks |

Cadence is driven by `tokio::time::sleep`. A `speed` multiplier in `CoreConfig`
(default 1.0) lets tests run at 100× without changing event ordering.

## 5. Content generation

Deterministic per `(session_id, turn_index)` via a seeded RNG, so re-running produces the
same transcript — required for stable screenshots and snapshot tests.

Responses must exercise the full markdown surface (F7): headings, paragraphs, bold/italic,
inline code, links, bullet and ordered and task lists, a blockquote, a table, and fenced
code blocks in several languages (Swift, Rust, TypeScript, Python, JSON, bash, diff). Some
responses are one short paragraph; some are 400 lines. Include at least one response with a
mid-stream unterminated fence so F7.3 is exercised live.

Tool calls draw from a realistic set — `read`, `write`, `edit`, `bash`, `grep`, `glob`,
`web_fetch` — with plausible arguments relative to the session's workspace root, and
results that include diff counts (`+268 -0`) for mutating tools so F1.3 renders.

## 6. Usage accounting

Token counts are estimated from generated content (chars/4 heuristic is fine, but be
consistent), and priced from the catalog (W4) so cost figures are internally consistent with
the Home dashboard. Emit `cache_read`/`cache_write` on later turns of a session so cache
effectiveness (F11.10) has data. Write one `turns` row and N `tool_invocations` rows per
turn through the store (W1) — the stats engine must never special-case stub data.

## 7. Concurrency

Multiple sessions can stream at once. Each run owns a task; a session may have at most one
active run (a second `sendPrompt` queues per F1.7 and is injected at the next
`turn_start`). `abortRun` flips the `AbortSignal` and the run must observe it between
events, not only between turns.

## 8. Done when

- `cargo test -p form-core harness::` asserts: exact event ordering, exactly one terminal
  event, partial accumulation equals the final message, abort latency under the speed
  multiplier, queued-message injection at a turn boundary, and deterministic output for a
  fixed seed.
- Two concurrent sessions interleave without cross-talk (assert by `sessionId` grouping).
