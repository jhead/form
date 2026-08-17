# Conventions

Rules for anyone — human or agent — changing this codebase. [README.md](README.md)
is the front door; [TODO.md](TODO.md) has port status and remaining work.

## The upstream tree is the specification

The TypeScript sources are the specification. They are referenced throughout as
`.upstream/` but are **not committed here**. Clone them if you need to read them:

```
git clone https://github.com/earendil-works/pi pi-rs/.upstream
```

When behaviour is in question, upstream decides, and a comment should say so. Where a port
deliberately differs, the divergence is commented at the site with the reason —
several already are, and they are load-bearing.

Keep module names tracking upstream file names (`api/anthropic-messages.ts` →
`src/anthropic_messages.rs`) so the two trees stay diffable.

## The consumer is Swift, over FFI

No FFI bindings live here, but the public API has to stay bridgeable. These are
not style preferences:

1. **No lifetimes or generic parameters in public API signatures.** Where
   upstream is generic over a TypeBox schema, carry `serde_json::Value`.
2. **Public types are owned, `'static`, `Send + Sync`, and serde-derivable.**
   JSON is the bridge format.
3. **Field names match upstream's wire shape** (`#[serde(rename_all = "camelCase")]`).
   This is a compatibility requirement, not a nicety — see the guarantees in
   TODO.md. Watch initialisms: `supportsOpenAIGrammarTools` does not round-trip
   through `rename_all`, and silently landed in a catch-all until a test caught it.
4. **Extension points are object-safe traits behind `Arc<dyn Trait + Send + Sync>`**,
   never generic bounds on public structs.
5. **Errors are flat enums with a stable `code() -> &'static str`.** Never leak
   `anyhow::Error` or `Box<dyn Error>` across a crate boundary.
6. **Async is `tokio`, and cancellation is an explicit `AbortSignal`** (see
   `pi_core::options`) — a Swift caller cannot drop a Rust future to cancel it.
7. **Streaming crosses the boundary as events, not iterators.** Producers use
   `AssistantMessageEventStream::channel`; consumers can use `for_each_event`.

## Crate layout

`pi-core` is the shared contract and every crate depends on it. **Additive
changes only** — a rename there touches everything. It depends on nothing else
in the workspace.

The dependency arrows are deliberate and worth preserving:

- `pi-catalog` does **not** depend on the provider crates. Adapters register at
  runtime by api id, so providers depend on the catalog and not the reverse.
- `pi-server` does **not** depend on `pi-agent` or `pi-session`. It expresses
  that dependency as the `SessionService` / `SessionRuntime` traits it owns.
- `pi-auth` does **not** depend on `pi-catalog`, which is why Copilot's
  model-policy list has to be injected by the caller.
- `pi-telemetry` depends on nothing in the workspace, matching upstream.

When something is needed in two crates that cannot see each other, hoist it into
`pi-core` rather than copying it. Two copies of the same logic have drifted here
twice already (`Model::rates_for`, `supported_thinking_levels`).

## Tests

- **Port upstream's tests, not just its code.** Several real bugs were caught
  only by translating an upstream test that asserted an ordering or a field
  position — evaluation order in a snapshot publisher, key order in a CBOR map.
- **No test may reach the network.** Provider adapters use `wiremock` with
  recorded fixtures under `tests/fixtures/`.
- **Adapters assert full event sequences**, including `contentIndex` values and
  the running `partial` snapshot, not just the final message.
- Never weaken or delete a test to make a change pass. If a test encodes an
  assumption that is genuinely wrong, fix the test and say so.

## Before you finish

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Dependency versions live in `[workspace.dependencies]` in the root `Cargo.toml`.
Add there first, then reference as `foo.workspace = true`.
