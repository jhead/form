# pi-rs

Rust port of the [Pi agent harness](https://github.com/earendil-works/pi) SDK.

This is a **library**, not a CLI. The upstream TUI, coding-agent CLI and eval
harness are out of scope; what is ported is the agent runtime, the multi-provider
LLM API, sessions, tools and the session protocol — the pieces you embed in
another application. The intended consumer is a Swift host talking to the crate
over FFI, which is why the public API avoids lifetimes and generics and keeps
everything owned, `Send + Sync` and serde-serializable.

See [TODO.md](TODO.md) for the port plan and current status, and
[AGENTS.md](AGENTS.md) for the conventions every crate follows.

## Status

Ported and green: `cargo test --workspace` → **1,543 passing**, clippy clean at
`-D warnings`. See [TODO.md](TODO.md) for per-crate detail, what was
deliberately left out (Bedrock, image generation, the Codex WebSocket
transport), and the cleanup backlog.

## Quick start

```rust
use pi_sdk::Pi;

let pi = Pi::builder().with_builtin_providers().build()?;
let model = pi.resolve_model("anthropic/claude-sonnet-4-5").await?;

let agent = pi.agent()
    .model(model)
    .system_prompt("You are terse.")
    .tools(pi_sdk::tools::default_tools())
    .env(std::sync::Arc::new(pi_sdk::tools::LocalExecutionEnv::new("/path/to/project")))
    .build()?;

agent.prompt_text("What changed in the last commit?", vec![]).await?;
```

`cargo run -p pi-sdk --example end_to_end` runs the whole stack offline — a
scripted provider driving the agent loop through a real tool call, persisted to
SQLite and reloaded.

## Using it over FFI

The SDK is written to be called from Swift. Beyond the ordinary async API:

- **`pi_sdk::blocking`** — a `Runtime` you own plus `prompt_text_blocking`,
  `prompt_text_streaming` (callback per event) and `prompt_text_background`
  (returns a cancel handle). A Swift caller cannot poll or drop a Rust future,
  which is also why every cancellable call takes an explicit `AbortSignal`
  rather than relying on future cancellation.
- **`pi_sdk::json`** — strings in, strings out, for hosts that would rather
  bridge one function than thirty. Failure is always a `Response::Error` value,
  never a panic across the boundary.
- Every public type is owned, `Send + Sync + 'static` and serde-serializable,
  with no lifetimes or generic parameters in public signatures. Errors are flat
  enums with a stable `code()`.

## Crates

| crate | what it is |
|---|---|
| `pi-core` | Shared wire types: messages, content, models, tools, the streaming event protocol, `ApiClient` |
| `pi-http` | HTTP/SSE transport shared by the provider adapters |
| `pi-catalog` | Model catalog and provider registry |
| `pi-auth` | Credential store, env resolution, OAuth flows |
| `pi-provider-common` | Message transform, option mapping, constrained sampling and cost logic every adapter shares |
| `pi-provider-anthropic` | `anthropic-messages` |
| `pi-provider-openai` | `openai-completions`, `openai-responses`, Azure, Codex |
| `pi-provider-google` | `google-generative-ai`, `google-vertex` |
| `pi-provider-misc` | `mistral-conversations`, `pi-messages`, the faux test provider |
| `pi-telemetry` | Telemetry contracts and the in-memory adapter |
| `pi-tools` | Built-in tools (bash, read, write, edit, search) and the fs/shell abstractions |
| `pi-session` | Session state, JSONL persistence, compaction |
| `pi-session-sqlite` | SQLite session backend |
| `pi-agent` | The agent loop and harness |
| `pi-protocol` | CBOR + framing for the session protocol |
| `pi-client` / `pi-server` | Session protocol over a Unix socket |
| `pi-sdk` | Facade that wires it all together |

## Compatibility

Three things are held byte-compatible with the TypeScript implementation, each
covered by fixture tests rather than left to good intentions:

- **Session JSONL files** — written by either implementation, readable by both.
- **The session protocol** — a Rust client can talk to a TypeScript server.
  Upstream's CBOR is deliberately not RFC 8949 canonical, so the fixtures are hex
  captured from upstream's own encoder.
- **The SQLite schema** — a database written by either implementation opens in
  the other.

See [TODO.md](TODO.md) for the details and for what is deliberately not ported.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

The upstream TypeScript sources are vendored read-only at `.upstream/` as the
port specification. [AGENTS.md](AGENTS.md) has the conventions — the FFI
constraints on the public API are load-bearing, not stylistic.

## License

MIT, matching upstream. See [LICENSE](LICENSE); the upstream copyright notice is
retained there as the MIT terms require.
