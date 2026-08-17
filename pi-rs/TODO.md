# Port status

Rust port of [earendil-works/pi](https://github.com/earendil-works/pi), scoped
to the SDK. [AGENTS.md](AGENTS.md) holds the conventions; [README.md](README.md)
is the front door. Upstream TypeScript is vendored read-only at `.upstream/` and
is the specification for everything here.

## Where it stands

The port is complete and green.

```
cargo test --workspace                             # 1543 passing, 0 failing
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

| Upstream | Crate | Tests | Notes |
|---|---|---|---|
| `packages/ai` core types | `pi-core` | 11 | The shared contract |
| `packages/ai` utils | `pi-http` | 213 | HTTP, SSE, retry, partial JSON, token estimation |
| `api/transform-messages` etc. | `pi-provider-common` | 55 | Shared adapter logic |
| `packages/ai` catalog | `pi-catalog` | 81 | 1283 models, 40 providers |
| `packages/ai` auth | `pi-auth` | 181 | 7 OAuth flows |
| `api/anthropic-messages` | `pi-provider-anthropic` | 82 | |
| `api/openai-*` | `pi-provider-openai` | 132 | 4 adapters |
| `api/google-*` | `pi-provider-google` | 89 | 2 adapters |
| `api/mistral`, `pi-messages`, faux | `pi-provider-misc` | 87 | |
| `packages/telemetry` | `pi-telemetry` | 23 | |
| `packages/agent` tools | `pi-tools` | 152 | |
| `packages/agent` sessions | `pi-session` | 102 | JSONL byte-verified |
| `packages/agent` loop | `pi-agent` | 110 | |
| `session-backends/sqlite-node` | `pi-session-sqlite` | 56 | Schema byte-verified |
| `packages/protocol` | `pi-protocol` | 62 | CBOR byte-verified |
| `packages/client` / `server` | `pi-client` / `pi-server` | 37 / 61 | Socket interop |
| — | `pi-sdk` | 9 | Facade + end-to-end example |

Out of scope by design, because this is a library: `packages/tui`,
`packages/coding-agent` (CLI, extensions, slash commands), `packages/evals`, and
the release tooling.

## Compatibility guarantees

Three things are held compatible with the TypeScript implementation, each
covered by fixture tests rather than good intentions:

1. **Session JSONL** — a file written by either implementation loads in the
   other. `pi-session` re-encodes a hand-built upstream fixture and asserts
   byte equality, and preserves unknown fields across a round trip.
2. **The session protocol** — `pi-protocol`'s fixtures are hex captured from
   upstream's *own* encoder run under Bun. Upstream's CBOR is deliberately not
   RFC 8949 canonical, so this is asserted at the byte level in both directions.
3. **The SQLite schema** — the migration SQL is a `diff`-verified copy applied
   through the same ledger, so a TS-written database opens here and vice versa.

Two more hold by construction: every wire type uses upstream's `camelCase`
field names, and each provider adapter asserts the full `AssistantMessageEvent`
sequence for a recorded payload, not just the final message.

## Not ported

Deliberate omissions, each cheap to add later because the surrounding types
already exist.

- **`bedrock-converse-stream`** — needs AWS SigV4 plus Smithy event-stream
  binary framing. `Api::BedrockConverseStream` exists in `pi-core`, so nothing
  else has to change when it lands.
- **Codex WebSocket transport** — SSE only; `transport: websocket*` falls
  through to SSE. The largest single gap in the adapters.
- **Image generation** (`images*.ts`, `openrouter-images`) — `ImagesModel` is in
  `pi-core`; the adapters are not ported.
- **Cloudflare gateway/workers bindings**, and `compat.ts`'s legacy global API.
- **GitHub Copilot dynamic headers** (`api/github-copilot-headers.ts`) — only
  the bearer-auth branch exists.
- **`AgentHarness` durable operations** — every one returns
  `HarnessNotImplemented`, because upstream's is also a scaffold
  (`agent-harness-scaffold.test.ts`). The vocabulary is ported so
  `pi-client`/`pi-server` can build against it.
- **Google safety settings, structured output, grounding** — upstream sends none
  of these; typed pass-through options exist instead of invented behaviour.

## Remaining work

- **`pi-http` gaps the Google adapter worked around.** No form-encoded request
  bodies (Google's OAuth token endpoint canonically wants
  `application/x-www-form-urlencoded`; it accepts JSON, so this is risk rather
  than breakage), and no raw byte-stream accessor beside `post_sse`. The second
  is a real correctness gap: `@google/genai` framing also splits on `\r\r` and
  can emit a bare un-prefixed JSON error body mid-stream, which the SSE layer
  silently skips. Add `HttpClient::post_bytes` and rewire the Google adapter —
  that also retires `pi-provider-misc`'s `support/http_stream.rs` shim.
- **`HttpError::Status` discards the raw error-body text.** Two adapters format
  their user-facing message from the untouched body and keep a transport shim to
  get at it. Add `body_text: String` and delete the shims.
- **GitHub Copilot model-policy acceptance** needs the model-id list injected
  into `pi-auth` via `with_policy_model_ids(...)`. Upstream reads its bundled
  catalog, which lives in `pi-catalog` here, and `pi-auth` must not depend on
  it — so the facade should wire the two together. Login works without it; it
  just skips policy acceptance.
- **`validate_tool_arguments` lives in `pi-http`.** Upstream has it in
  `packages/ai`, which maps to `pi-core`. It works where it is and moving it
  would churn three crates for no behavioural gain, so it stays until a third
  crate needs it.
- **`pi-agent` depends on `pi-session`** purely to re-export `AgentMessage`. If
  the agent loop is ever wanted without the session layer, that union belongs in
  `pi-core` next to `Message`.

### Duplication that is deliberate

Recorded so a future reader does not "fix" it:

- **`resolve_cache_retention`** exists in three adapters. Upstream duplicates it
  per adapter file too, and the `pi-messages` variant genuinely returns
  `Option<CacheRetention>`.
- **Two `pi_user_agent` renderings.** `pi-provider-openai` emits
  `pi (<os> <release>; <arch>)` per `utils/pi-user-agent.ts`; `pi_http::headers`
  emits `pi (<os> <arch>)`. Different strings, not a stale copy.
- **Two `format_provider_error`.** They take different inputs — a folded
  `AiError` vs a `NormalizedProviderError`.

## Refreshing the model catalog

`crates/pi-catalog/data/` holds the generator's output, not a re-derivation, so
it carries all of upstream's provider-specific corrections. To refresh:

```bash
cd .upstream && npm install --ignore-scripts
npm --prefix packages/ai run generate-models
# then copy packages/ai/src/providers/data/*.json into crates/pi-catalog/data/
```

See `crates/pi-catalog/README.md` for the full procedure. `.upstream/node_modules`
is gitignored and safe to delete when not regenerating.
