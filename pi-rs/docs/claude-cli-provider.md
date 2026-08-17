# A `claude` CLI provider for pi-rs

Feasibility study. **No implementation exists; nothing here has been built.**

Tested against **Claude Code `2.1.220`** (`claude --version`), native arm64 build at
`~/.local/share/claude/versions/2.1.220`, on macOS 15 (Darwin 24.6.0), with a
`claude.ai` login on a **Pro** subscription. Every wire-format claim below is
backed by a capture in
`/private/tmp/claude-501/-Users-jhead-dev/59c70a2c-432d-4355-bad1-213be9ce5675/scratchpad/fixtures/`
(referenced per-claim as `NN-name.jsonl`). Claims I could not run are marked
**unverified**.

---

## 1. Feasibility verdict

**It is feasible, and more feasible than expected — but only in a specific
configuration, and it is the wrong thing to build first.**

The narrow verdict: `claude -p --output-format stream-json
--include-partial-messages` emits the **raw Anthropic Messages SSE events
verbatim**, wrapped one-per-line in a `{"type":"stream_event","event":{…}}`
envelope. Combined with `--system-prompt` (full replacement, not append) and
`--tools ""` (removes every built-in tool), the CLI can be reduced to something
very close to a single stateless model turn: measured input overhead dropped
from **~9,069 tokens to 174 tokens** between `01-print-json-notools.json`
(default prompt) and `02-stream-json-partial.jsonl` (`--system-prompt "You are
terse."`). pi's own tools can be injected through `--mcp-config` and are visible
to the model as real tool definitions (`04-mcp-injected-tool.jsonl`, where
`system/init` reports `tools: ["mcp__pi__pi_echo"]` and nothing else). An
`ApiClient` implementation that maps this onto `AssistantMessageEvent` is
straightforward, because `pi-provider-anthropic` already parses exactly these
SSE events.

The hard limits, stated plainly and expanded in §5: **the CLI always executes
tool calls itself** — `--max-turns 1` does not prevent this
(`06-max-turns-1.jsonl`: the MCP tool ran, *then* the run stopped with
`subtype: "error_max_turns"`); **prior assistant turns cannot be seeded** — an
`{"type":"assistant"}` message on stdin is silently discarded and produces no
turn and no output at all (`07-assistant-seed.jsonl`, 0 bytes); and there is no
way to set `temperature`, `max_tokens`, sampling params, thinking budgets,
cache retention, or per-request headers.

**And the decisive point, which reframes the whole exercise: pi-rs already
supports subscription billing without the CLI.**
`crates/pi-auth/src/oauth/anthropic.rs` is a complete, ported Claude Pro/Max
OAuth flow (`https://claude.ai/oauth/authorize`, PKCE, loopback on port 53692,
scopes including `user:inference user:sessions:claude_code`), it reports
`is_subscription() == true`, and `crates/pi-provider-anthropic/src/request.rs`
already handles OAuth tokens end-to-end: `is_oauth_token()` detects
`sk-ant-oat*`, and `build_params` prepends the required
`"You are Claude Code, Anthropic's official CLI for Claude."` system block and
canonicalises tool names to Claude Code's casing. `ANTHROPIC_PROVIDER` lists
`ANTHROPIC_OAUTH_TOKEN` ahead of `ANTHROPIC_API_KEY` in `api_key_env` and sets
`supports_oauth: true`.

So the honest verdict is: **a CLI provider is buildable as a constrained
single-turn transport, but the user's actual goal — "bill against my
subscription" — is already met by the existing `anthropic-messages` adapter
plus the existing OAuth login, with the *full* `ApiClient` contract intact.**
See §6.

---

## 2. The mechanism

### 2.1 What the CLI offers

From `claude --help` (captured verbatim in `fixtures/claude-help.txt`) and
[the headless docs](https://code.claude.com/docs/en/headless) /
[CLI reference](https://code.claude.com/docs/en/cli-reference):

| Flag | Why it matters here |
|---|---|
| `-p, --print` | Non-interactive. No TUI, no terminal scraping. |
| `--output-format stream-json` | NDJSON on stdout. The machine-readable interface. |
| `--include-partial-messages` | Adds `stream_event` lines carrying raw Anthropic SSE. Requires `-p` + `stream-json`. |
| `--verbose` | Required for `stream-json` to emit anything but the result line. |
| `--input-format stream-json` | NDJSON on stdin: a persistent session fed user messages. |
| `--system-prompt` / `--system-prompt-file` | **Replaces** the entire Claude Code system prompt. |
| `--append-system-prompt` | Appends instead. Not what we want. |
| `--tools ""` | Removes every built-in tool from the request. |
| `--mcp-config <json-or-file>` + `--strict-mcp-config` | Injects arbitrary tool definitions; ignores all other MCP config. |
| `--allowedTools "mcp__pi__*"` | Auto-approves so nothing prompts. |
| `--permission-mode dontAsk` | Hard-denies anything not pre-approved. Safer than `bypassPermissions`. |
| `--model <alias-or-id>` | `sonnet`, `opus`, `haiku`, `fable`, or a full id. |
| `--effort low\|medium\|high\|xhigh\|max` | The only thinking control. Not a token budget. |
| `--session-id <uuid>` / `--resume` / `--fork-session` | Session continuity. |
| `--no-session-persistence` | Don't write JSONL to `~/.claude/projects`. Print mode only. |
| `--max-turns N` | Caps agent turns. **Does not stop tool execution** — see §5. |
| `--max-budget-usd` | Client-side spend cap. |
| `--json-schema` | Structured output after the loop completes. |
| `--settings <file-or-json>` | Session-scoped settings override. |
| `--safe-mode` | Disables CLAUDE.md, skills, plugins, hooks, MCP, custom agents. Auth still works. |
| `--bare` | **Do not use.** Explicitly never reads OAuth or the keychain — see §3. |

Undocumented in `--help` but present in 2.1.220, verified by option-parser
probing (`claude -p --max-turns 1` → "Input must be provided…", i.e. the flag
parsed; `claude -p --nonexistent-flag` → `error: unknown option`):
`--max-turns`, `--permission-prompt-tool`, `--system-prompt-file`,
`--append-system-prompt-file`, `--init`. `--quiet` does **not** exist.

### 2.2 The invocation

The minimum-overhead, maximum-fidelity form:

```bash
claude -p "<prompt>" \
  --output-format stream-json \
  --include-partial-messages \
  --verbose \
  --system-prompt "<pi Context.system_prompt>" \
  --tools "" \
  --model sonnet \
  --no-session-persistence \
  --permission-mode dontAsk
```

With pi's tools injected:

```bash
claude -p "<prompt>" \
  --output-format stream-json --include-partial-messages --verbose \
  --system-prompt "<pi Context.system_prompt>" \
  --tools "" \
  --mcp-config '{"mcpServers":{"pi":{"type":"stdio","command":"…","args":[…]}}}' \
  --strict-mcp-config \
  --allowedTools "mcp__pi__toolname" \
  --permission-mode dontAsk \
  --no-session-persistence --model sonnet
```

`--mcp-config` also accepts `http`/`sse` server entries, so the bridge can be an
in-process Rust HTTP server on loopback rather than a second executable — which
matters a great deal for the design in §4.3.

### 2.3 The wire format

Every stdout line is one JSON object. Observed top-level `type` values across
all captures: `system` (subtypes `init`, `status`, `post_turn_summary`;
documented also `api_retry`, `plugin_install`, hook events), `stream_event`,
`assistant`, `user`, `rate_limit_event`, `result`.

**`system/init`** — first line, session metadata. From
`02-stream-json-partial.jsonl`, trimmed:

```json
{"type":"system","subtype":"init","cwd":"…","session_id":"801b6e9f-…",
 "tools":[],"mcp_servers":[…],"model":"claude-sonnet-5",
 "permissionMode":"default","apiKeySource":"none",
 "claude_code_version":"2.1.220","output_style":"default",
 "capabilities":["interrupt_receipt_v1","interrupt_cancel_queued_v1","msg_lifecycle_v1"],
 "uuid":"0bf8fa01-…"}
```

`apiKeySource` is the billing tell (§3). `capabilities` is the documented
feature-detection channel — prefer it to version comparison.

**`stream_event`** — the payload that makes this design work. The `event`
object is a **verbatim Anthropic Messages SSE event**:

```json
{"type":"stream_event","event":{"type":"message_start","message":{"model":"claude-sonnet-5","id":"msg_011Ce7cm8mYTxFHexQQBurF3","type":"message","role":"assistant","content":[],"stop_reason":null,"usage":{"input_tokens":174,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1,"service_tier":"standard"}}},"session_id":"801b6e9f-…","parent_tool_use_id":null,"uuid":"ba1a6239-…","ttft_ms":692}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},…}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"h"}},…}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ello"}},…}
{"type":"stream_event","event":{"type":"content_block_stop","index":0},…}
{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"input_tokens":174,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":4,"output_tokens_details":{"thinking_tokens":0}}},…}
{"type":"stream_event","event":{"type":"message_stop"},…}
```

`message_start` / `content_block_start` / `content_block_delta` /
`content_block_stop` / `message_delta` / `message_stop` with
`text_delta` — this is the exact event vocabulary
`crates/pi-provider-anthropic/src/anthropic_messages.rs` already consumes.
`output_tokens_details.thinking_tokens` is present, which is the field pi's
`Usage.reasoning` wants.

**`assistant`** — the complete message for each step, including tool calls
(`03-tooluse-streamjson-input.jsonl`):

```json
{"type":"assistant","message":{"model":"claude-sonnet-5","id":"msg_…","role":"assistant",
 "content":[{"type":"tool_use","id":"toolu_01NQcQhK6hSYfPkx81c5PPWZ","name":"Read",
             "input":{"file_path":"./probe.txt"},"caller":{"type":"direct"}}],
 "usage":{…}},
 "parent_tool_use_id":null,"session_id":"…","uuid":"…",
 "timestamp":"2026-08-17T03:34:14.365Z","request_id":"req_…"}
```

**`user`** — the CLI feeding *itself* the tool result. This is the loop we do
not own:

```json
{"type":"user","message":{"role":"user","content":[
   {"tool_use_id":"toolu_01NQcQhK6hSYfPkx81c5PPWZ","type":"tool_result",
    "content":"1\tthe secret is 42\n2\t"}]},
 "parent_tool_use_id":null,"session_id":"…","tool_use_result":{…}}
```

**`rate_limit_event`** — subscription quota, surfaced machine-readably. Two
shapes observed during this study:

```json
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1786945800,
  "rateLimitType":"five_hour","overageStatus":"rejected",
  "overageDisabledReason":"org_level_disabled","isUsingOverage":false},…}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1786945800,
  "rateLimitType":"five_hour","utilization":0.9,"isUsingOverage":false},…}
```

**`result`** — always last. From `01-print-json-notools.json`:

```json
{"is_error":false,"duration_api_ms":2809,"num_turns":1,"stop_reason":"end_turn",
 "session_id":"d2917bc3-…","total_cost_usd":0.0363077,
 "usage":{"input_tokens":2,"cache_creation_input_tokens":5780,"cache_read_input_tokens":3289,
          "output_tokens":4,"cache_creation":{"ephemeral_1h_input_tokens":5780,"ephemeral_5m_input_tokens":0},
          "iterations":[…]},
 "modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":520,"outputTokens":11,"costUSD":0.000575,…},
               "claude-sonnet-5":{"inputTokens":2,"outputTokens":4,"cacheReadInputTokens":3289,
                                  "cacheCreationInputTokens":5780,"costUSD":0.0357327,
                                  "contextWindow":1000000,"maxOutputTokens":64000,
                                  "canonicalModel":"claude-sonnet-5","provider":"firstParty"}},
 "permission_denials":[],"terminal_reason":"completed","subtype":"success",
 "api_error_status":null,"result":"hello","type":"result","duration_ms":2521}
```

`stop_reason` on the result maps onto pi's `DoneReason`; `subtype` distinguishes
`success` from `error_max_turns`, `error_max_budget_usd`,
`error_during_execution`. Note the stray **`claude-haiku-4-5` entry in
`modelUsage` on every single run** — 520 input tokens for the
`system/post_turn_summary` classifier. It is billed and I found no flag to
suppress it (see §5).

### 2.4 Streaming input

`--input-format stream-json` accepts one JSON object per line. Verified
accepted and turn-producing:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"…"}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_…","content":"…"}]}}
```

Verified **silently ignored** (zero bytes of output, no `system/init`, exit 0):

```json
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Prior turn."}]}}
```

That single negative result (`07-assistant-seed.jsonl`) is the most
consequential finding in this document. It is what stops the provider from
being a clean stateless turn.

---

## 3. Authentication

### 3.1 How subscription auth actually resolves

`claude auth status` is a machine-readable probe and needs no API call:

```json
{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
 "email":"…","orgId":"f3c20bb0-…","orgName":"…","subscriptionType":"pro"}
```

The credential is an OAuth token in the macOS keychain, written by `claude auth
login`. **It is looked up under an account name derived from `$USER`.** Verified
by bisection:

| environment | `loggedIn` |
|---|---|
| `env -i HOME PATH` | `false` |
| `env -i HOME PATH USER` | **`true`** |
| `env -i HOME PATH LOGNAME` | `false` |
| `env -i HOME PATH SHELL` | `false` |
| `env -i HOME PATH TMPDIR` | `false` |
| `env -i HOME PATH CLAUDE_CODE_MESSAGING_SOCKET CLAUDE_CODE_MESSAGING_TOKEN` | `false` |

A subprocess spawned with a scrubbed environment **loses subscription auth
silently** and fails with `"result":"Not logged in · Please run /login"` inside
an otherwise well-formed `result` object (`is_error: true`,
`terminal_reason: "api_error"`, `stop_reason: "stop_sequence"`, exit status 1) —
captured as `00-not-logged-in-scrubbed-env.json`. Note that it arrives as a
well-formed `result` line on **stdout**, not as a crash or a stderr message, so
the provider must key off `result.is_error` / `terminal_reason` and
`system/init.apiKeySource`, not just the exit code. Any provider that scrubs the
environment for hygiene must keep `HOME`, `PATH`, and `USER`.

### 3.2 `ANTHROPIC_API_KEY` — does it silently switch billing?

**On 2.1.220, with an existing `claude.ai` login: no. The OAuth subscription
credential wins.** Verified by setting a *deliberately invalid* key:

```
ANTHROPIC_API_KEY="sk-ant-api03-BOGUS-DOES-NOT-EXIST" claude auth status
→ {"loggedIn":true,"authMethod":"claude.ai","apiKeySource":"ANTHROPIC_API_KEY",…}

ANTHROPIC_API_KEY="sk-ant-api03-BOGUS-DOES-NOT-EXIST" claude -p "hi" …
→ system/init: "apiKeySource":"ANTHROPIC_API_KEY"
→ assistant: "Hi! How can I help you today?"
→ rate_limit_event: {"rateLimitType":"five_hour","utilization":0.9,…}
```

(`05-with-api-key-env.jsonl`.) A bogus key cannot produce a successful
completion, and a `five_hour` `rate_limit_event` is a subscription-only
artefact. So the request went out on the OAuth token. **But `apiKeySource`
reported `ANTHROPIC_API_KEY`**, which is actively misleading, and I would not
build on this precedence order: it is undocumented, version-sensitive, and the
opposite has been true in past Claude Code releases. **Treat it as unverified
for any version other than 2.1.220.**

The defensive rule for the provider: **remove `ANTHROPIC_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_MODEL`,
`CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX` from the child environment,
then assert `system/init.apiKeySource == "none"` on the first line and abort the
stream with an `Error` event if it is anything else.** That is a cheap,
per-request, machine-checkable billing guarantee, and it is the only one
available. Note that this host's own environment ships `ANTHROPIC_BASE_URL` set,
so inheriting the parent environment wholesale is not safe.

### 3.3 Things that break subscription auth

- **`--bare`.** The help text is explicit: *"Anthropic auth is strictly
  `ANTHROPIC_API_KEY` or `apiKeyHelper` via `--settings` (OAuth and keychain are
  never read)."* The docs repeat it: *"bare mode doesn't use your subscription
  login."* Despite `--bare` being the documented recommendation for scripted and
  SDK calls, **this design must not use it.** Use `--safe-mode` instead for
  configuration isolation; it disables CLAUDE.md, skills, plugins, hooks and MCP
  while leaving auth alone. (`--safe-mode` was not exercised in a live run —
  **unverified** that it composes cleanly with `--mcp-config`.)
- A scrubbed `USER` (§3.1).
- **Unverified:** whether the keychain read prompts on first access from a
  differently-signed parent process. A Swift `.app` spawning `claude` may hit a
  keychain ACL dialog that a terminal-launched process does not. This must be
  tested on a signed, sandboxed build before committing to the design.

### 3.4 What the Swift host must do

1. Ship no credential of its own. Require the user to have run `claude auth
   login` in a terminal — the login is an interactive browser PKCE flow and
   cannot be driven from a subprocess.
2. Locate the binary explicitly (`~/.local/bin/claude` is a symlink into
   `~/.local/share/claude/versions/<v>`); do not rely on `PATH` inheritance from
   a GUI app, where `PATH` is `/usr/bin:/bin:/usr/sbin:/sbin`.
3. Pass `HOME`, `PATH`, `USER` explicitly; drop the `ANTHROPIC_*` set.
4. If the app is sandboxed, spawning arbitrary subprocesses and reaching the
   user's keychain item is not permitted under the standard App Sandbox. **This
   design is incompatible with a Mac App Store sandboxed build** — unverified in
   detail, but the sandbox rules on subprocess execution and keychain ACLs make
   it very unlikely to work without entitlements Apple does not grant.
5. Surface `claude auth status` output in the UI so the failure mode is legible.

### 3.5 `claude setup-token` — the bridge to the better design

`claude setup-token` ("Set up a long-lived authentication token (requires Claude
subscription)") mints an `sk-ant-oat*` token. `pi-provider-anthropic`'s
`is_oauth_token()` matches exactly that prefix, and `ANTHROPIC_PROVIDER`'s
`api_key_env` already lists `ANTHROPIC_OAUTH_TOKEN` first. So the existing
adapter consumes that token directly, with no subprocess at all. Not run here
(it creates a durable credential and needs a browser).

---

## 4. Technical approach

### 4.1 Crate layout

```
crates/pi-provider-claude-cli/
  src/
    lib.rs          # api id "claude-cli", ProviderDescriptor
    provider.rs     # descriptor, model list, catalog registration
    process.rs      # spawn, env scrubbing, stdin/stdout framing, lifecycle
    protocol.rs     # serde types for the NDJSON envelopes
    translate.rs    # CLI stream -> AssistantMessageEvent
    bridge.rs       # (scope B only) loopback MCP server exposing Context.tools
```

`pi-catalog` need not change: `ModelRegistry::register_api` takes an
`ApiClientRef` at runtime, and `pi-catalog` deliberately does not depend on
provider crates. Wire it into `PiBuilder::with_builtin_providers` behind a
non-default cargo feature (it shells out; it should not be on by default).

Reuse: `translate.rs` should not reimplement SSE handling. The `stream_event`
payloads are byte-identical in shape to what
`crates/pi-provider-anthropic/src/anthropic_messages.rs` already handles. Per
AGENTS.md ("when something is needed in two crates that cannot see each other,
hoist it into `pi-core`"), the right move is to extract the Anthropic
SSE-event → `AssistantMessageEvent` state machine into a shared home —
`pi-provider-common` is the natural one, since both provider crates already
depend on it — rather than forking a second copy. Two copies of the same logic
have already drifted twice in this tree.

### 4.2 Event mapping

| CLI line | `AssistantMessageEvent` |
|---|---|
| `system/init` | validate `apiKeySource`, record `session_id`; emit `Start { partial }` |
| `stream_event: message_start` | seed `partial.response_id`, `response_model`, input/cache usage |
| `stream_event: content_block_start {text}` | `TextStart { content_index }` |
| `stream_event: content_block_delta {text_delta}` | `TextDelta { content_index, delta, partial }` |
| `stream_event: content_block_start {thinking}` | `ThinkingStart` *(unverified — see §5)* |
| `stream_event: content_block_delta {thinking_delta}` | `ThinkingDelta` *(unverified)* |
| `stream_event: content_block_start {tool_use}` | `ToolCallStart` |
| `stream_event: content_block_delta {input_json_delta}` | `ToolCallDelta` *(unverified)* |
| `stream_event: content_block_stop` | `TextEnd` / `ThinkingEnd` / `ToolCallEnd` by block kind |
| `stream_event: message_delta` | update `stop_reason`, `output_tokens`, `thinking_tokens` |
| `rate_limit_event` | `AssistantMessageDiagnostic { code: "subscription_rate_limit", severity }` |
| `system/api_retry` | `AssistantMessageDiagnostic { code: "stream_retry" }` (matches the existing code) |
| `system/post_turn_summary` | drop |
| `result subtype=success` | `Done { reason, message }` with `Usage` from `modelUsage` |
| `result subtype=error_*` / `is_error` | `Error { reason: Error, error }` with `error_message` from `result` |
| process died before `result` | `Error { reason: Error, … }` — never a bare `Err` |

`content_index` must be pi's own running index, not the CLI's SSE `index`, and
AGENTS.md requires adapter tests to assert full event sequences including
`contentIndex` and the running `partial` snapshot. Fixtures for those tests
already exist in the scratchpad; they should be copied to
`crates/pi-provider-claude-cli/tests/fixtures/`. No test may reach the network,
so the tests must replay recorded NDJSON through the translator, not spawn
`claude`.

`Usage` should come from `result.modelUsage` summed across models, not
`result.usage`: the docs are explicit that `usage` excludes subagent tokens
while `modelUsage` includes them. `Cost` should be recomputed from pi's own
`Model::rates_for` rather than trusting `total_cost_usd`, which the docs label a
client-side estimate not to be used for financial decisions — and which is
meaningless anyway on a subscription, where nothing is charged per token.

### 4.3 The central tension: who owns the loop

Three options, weighed.

**(a) Surface the CLI's tool calls as pi tool calls and let pi's loop drive
them.** Requires the blocking-MCP-bridge, and it *does* work mechanically. My
stub MCP server (`fixtures/mcp_stub.py`) deliberately slept 3 s inside
`tools/call`; the CLI blocked for the full duration and then continued
(`04-mcp-injected-tool.jsonl`). So the bridge can hold the CLI hostage while
pi's agent loop executes the tool for real.

The problem is the seam with `ApiClient`. When the bridge receives
`tools/call`, the provider must emit `ToolCallEnd` + `Done { ToolUse }` and
close the pi stream — but the CLI subprocess is still alive, blocked on an
unanswered JSON-RPC request. pi's loop then executes the tool and calls
`stream()` *again* with a `Context` containing the `ToolResultMessage`. The
provider must recognise that this new call continues the suspended process,
answer the blocked `tools/call` with the result, and resume translating.
`StreamOptions.session_id` is the only correlation key available, so the whole
scheme hangs on the agent loop passing a stable `session_id` — and on a process
registry keyed by it, with timeouts, orphan reaping, and a policy for what
happens when pi sends a `Context` that has diverged from what the CLI's session
believes. This is real statefulness smuggled behind a trait documented as a
single turn. It is buildable. I would not build it.

**(b) Let the CLI own the loop and flatten its output.** Ignore
`Context.tools`, let the CLI use its own built-ins, and emit the whole
multi-turn trajectory as one enormous pi assistant message with the CLI's tool
calls rendered as text. This is simple and honest about being a different
product, but it is not an `ApiClient` — it is an agent pretending to be a model.
pi's tools, permissions, session persistence, compaction and telemetry all
become dead weight, and the two agent loops fight over context management. It
also puts the CLI's built-in Bash/Edit/Write tools inside pi's process with a
permission model pi does not control.

**(c) Refuse the loop entirely: single-turn, tools injected, loop terminated at
the first tool call.** `--tools ""` plus `--mcp-config` gives the model exactly
pi's tool set and nothing else. The bridge answers `tools/call` **immediately**
with a sentinel that makes the CLI stop — and then the provider kills the
process, having already emitted `ToolCallEnd` + `Done { ToolUse }`. pi's loop
executes the tool for real and issues a fresh `stream()` with the full
conversation, which starts a *new* CLI process. Stateless per call. No process
registry, no session correlation, no hostage subprocess.

The cost of (c) is the `07-assistant-seed.jsonl` finding: since prior assistant
turns cannot be seeded, every call must re-render pi's entire `Context.messages`
into the single user-message prompt as a transcript — role-labelled text with
tool calls and results serialised inline. That is lossy (thinking signatures,
`toolCall.id` continuity, images-as-blocks all degrade), re-pays the full input
token cost every turn with no prefix cache hit on the model's real conversation
prefix, and means the model sees its own prior turns as *quoted text a user
pasted* rather than as its own assistant turns. On long conversations that is a
material behavioural difference, not a cosmetic one.

**Recommendation among the three: (c)**, and only (c). It is the only one that
keeps `ApiClient`'s stateless-turn contract honest, keeps pi's agent loop in
charge of tool execution, and keeps the CLI's built-in tools out of the picture
entirely. (a) trades a large amount of hidden state for a marginal fidelity
gain; (b) is a different product wearing the trait as a costume.

Note that (c)'s sentinel-stop is **unverified**. I confirmed the CLI blocks on
the MCP response and I confirmed `--max-turns 1` does not prevent tool
execution, but I did not test which MCP error response (or SIGTERM timing)
produces the cleanest stop without the model getting a chance to react to a
bogus tool result. That is the first thing a prototype must nail down.

### 4.4 Process lifecycle and cancellation

- Spawn with `tokio::process::Command`, `stdout` piped, `stderr` piped and
  captured into diagnostics, `stdin` closed (scope A) or piped (if streaming
  input is used). Set `kill_on_drop(true)`.
- Read stdout with a `LinesCodec`; each line is one JSON object. Lines can be
  large — the `system/init` line alone was ~2 KB here and grows with installed
  plugins and skills. Do not assume a small max line length.
- Feed events into `AssistantMessageEventStream::channel`, per AGENTS.md rule 7.
- **Cancellation** maps cleanly onto AGENTS.md rule 6: `tokio::select!` on
  `options.request.signal().aborted()`. The docs specify SIGTERM behaviour
  precisely — *"Claude Code aborts the in-progress turn, terminates the process
  tree of any running Bash command, runs SessionEnd hooks, and exits with code
  143."* So: on abort, SIGTERM the child, wait a bounded grace period, then
  SIGKILL, and emit `Error { reason: Aborted }`. Because we run `--tools ""`
  there is no Bash process tree to worry about.
- `--no-session-persistence` avoids littering `~/.claude/projects` and avoids a
  disk write per turn.
- The docs warn that Claude Code waits up to 30 s for a slow stdout consumer
  before exiting; the reader must drain promptly or teardown stalls.
- Startup latency is real: `time_to_request_ms` was 8–326 ms across runs, on top
  of native binary startup. Every `stream()` call pays it. Unverified how much
  worse this gets with a large plugin/MCP set — `--safe-mode` or
  `--strict-mcp-config` should be used to bound it.

### 4.5 FFI constraints (AGENTS.md)

Nothing here strains the rules. The `ApiClient` impl is a plain struct behind
`Arc<dyn ApiClient>` (rule 4); errors become a flat enum with `code()` (rule 5);
cancellation is the explicit `AbortSignal` (rule 6); output crosses as events
via the channel sink (rule 7); no lifetimes or generics leak (rule 1). The one
new wrinkle is that a Swift host now depends on an external binary's presence,
version and login state — that is a runtime precondition the FFI surface should
expose as an explicit probe (a `claude auth status` wrapper returning a
serde-serializable struct) rather than something discovered as a stream error on
the first prompt.

There is no upstream TypeScript counterpart for this crate, so AGENTS.md's
"upstream is the specification" rule has nothing to say. That should be
commented at the crate root as a deliberate divergence.

---

## 5. Caveats — what is NOT possible

Exhaustive, and deliberately unsoftened.

### 5.1 `ApiClient` / `StreamOptions` surface that cannot be honored

| Contract element | Status |
|---|---|
| `StreamOptions.temperature` | **Impossible.** No CLI flag. Silently ignored. |
| `StreamOptions.max_tokens` | **Impossible.** No flag. The model's default cap applies (`maxOutputTokens: 64000` for sonnet-5 in `modelUsage`). |
| `StreamOptions.sampling_params` | **Impossible.** No flag. |
| `StreamOptions.cache_retention` | **Impossible.** The CLI manages caching itself. `ENABLE_PROMPT_CACHING_1H` exists but the docs say subscription users get 1 h automatically, so the knob is not ours. |
| `StreamOptions.transport` | Meaningless. No WebSocket path. |
| `StreamOptions.websocket_connect_timeout_ms` | Meaningless. |
| `StreamOptions.metadata` | **Impossible.** No pass-through. |
| `RequestOptions.headers` | **Impossible.** No way to set request headers. `--betas` exists but is explicitly "API key users only". |
| `RequestOptions.api_key` | **Must be ignored.** Honoring it would switch billing off the subscription — the exact opposite of the point. |
| `RequestOptions.timeout_ms` | Implementable only as a wall-clock kill of the subprocess, not a request timeout. |
| `RequestOptions.max_retries` / `max_retry_delay_ms` | **Not ours.** The CLI retries internally and reports `system/api_retry`. Cannot be configured. |
| `RequestOptions.on_payload` | **Impossible.** There is no payload to intercept or rewrite. |
| `RequestOptions.on_response` | **Impossible.** No HTTP status or headers are exposed. |
| `RequestOptions.env` | Partially honorable, but dangerous — see §3.2. Most `ANTHROPIC_*` values must be *stripped*, not forwarded. |
| `SimpleStreamOptions.reasoning` (`ThinkingLevel`) | **Coarsely.** Maps onto `--effort low\|medium\|high\|xhigh\|max` only. |
| `SimpleStreamOptions.thinking_budgets` | **Impossible.** No token budget exists; effort is a categorical. |
| `supports_deferred` / `fetch_deferred` / `cancel_deferred` | **Impossible.** No batch/deferred surface. Must stay `false`. |
| `Model.base_url` | Ignored. Setting `ANTHROPIC_BASE_URL` would redirect the CLI off first-party and break subscription billing. |
| `Model.headers` | Ignored. |
| `Model.compat` (`AnthropicMessagesCompat`) | Entirely inapplicable — we do not build the request. |
| `AbortSignal` | **Honorable.** SIGTERM → documented abort, exit 143. |
| `Usage` | **Partially.** Token counts arrive; `Cost` is a client-side estimate that means nothing on a subscription. `cache_write_1h` is derivable from `cache_creation.ephemeral_1h_input_tokens`. |

### 5.2 Loop and tool-model limits

- **The CLI always executes tool calls itself.** Verified: `--max-turns 1` with
  an MCP tool still ran the tool, then terminated with `subtype:
  "error_max_turns"`, `stop_reason: "tool_use"`, `num_turns: 2`
  (`06-max-turns-1.jsonl`). There is no mode where the CLI hands a tool call back
  to the caller for execution. `--permission-prompt-tool` and the SDK's
  `canUseTool` let you *approve or deny* a call; they do not let you *serve* it.
  Every design that gives pi's loop control of tool execution therefore routes
  through an MCP server that impersonates the tool.
- **Tool names are namespaced.** An injected tool `read` becomes
  `mcp__pi__read` in the model's view and in every `tool_use` block. The
  provider must strip the prefix on the way out and re-add it on the way in.
  This collides with pi's `from_claude_code_name` convention in the existing
  adapter and needs its own mapping.
- **Tool JSON Schemas survive, but through MCP's `inputSchema`,** which is a
  narrower dialect than what pi's `Tool.parameters` can hold. `Tool.constrained_sampling`
  (`ConstrainedSamplingConfig::JsonSchema { strict }`, `Grammar { variants }`) has
  **no representation at all** — no strict-mode, no Lark, no regex grammars.
- **Prior assistant turns cannot be injected.** Verified (§2.4). Conversation
  history must be flattened into a user-message transcript, which loses
  assistant-role framing, `thinkingSignature` continuity, `textSignature`, and
  stable `toolCall.id` linkage across turns.
- **Images.** `Context.messages` can carry `InputContent::Image`; the
  stream-json input format accepts base64 image blocks in a user message, so
  images survive *if* streaming input is used. Under scope (c)'s
  transcript-flattening they would need to be attached to the single user
  message. **Unverified** — not tested.
- **`Context.system_prompt` is honorable** via `--system-prompt`, and this is
  the single most important flag in the design. But it is a *full replacement*:
  it removes Claude Code's tool-use conventions along with everything else, and
  the model's behaviour with injected MCP tools under a foreign system prompt is
  **unverified at length** (my test used a one-line prompt and a trivial tool).
- **`--tools ""` does not eliminate all overhead.** With a replacement system
  prompt and no tools, input was 174 tokens (`02`). With one built-in tool
  (`Read`) enabled it jumped to 7,792 cache-creation tokens (`03`), and with the
  default system prompt it was ~9,069 (`01`). The cliff between "no tools" and
  "any tool" is steep and I did not isolate its cause. With one *MCP* tool it
  was only 633 (`04`), so the penalty appears specific to built-ins — but treat
  the exact numbers as indicative, not contractual.

### 5.3 Unavoidable extra billing

- **Every run makes an extra `claude-haiku-4-5` call.** Present in all four
  successful captures — 520 input / 11 output tokens, `costUSD` ≈ $0.000575 —
  feeding the `system/post_turn_summary` line. There is no documented flag to
  disable it. On a subscription this is small but it is not zero, it is
  per-`stream()`-call, and pi's agent loop calls `stream()` once per turn.
- **Subagents, hooks, skills, plugins and project MCP servers load by default in
  `-p` mode** and can each cost tokens. The docs warn that `-p` runs a project's
  `.claude/settings.json` hooks and connects its `.mcp.json` servers *without a
  trust prompt*. `--strict-mcp-config` and `--safe-mode` are mandatory hardening,
  not optional.

### 5.4 Operational and security

- **A subprocess per model turn.** Native-binary startup plus MCP connect on
  every `stream()`. `time_to_request_ms` alone ranged 8–326 ms here; total
  `duration_ms` for "say hello" was 2.1–2.5 s.
- **Version coupling.** Every finding here is pinned to 2.1.220. The CLI
  auto-updates. `system/init.claude_code_version` and `capabilities` must be
  checked at runtime, and the provider should refuse to run below a known-good
  floor rather than mistranslate.
- **`--permission-mode bypassPermissions` must never be used**, even though it
  is convenient. With `--tools ""` there is nothing to bypass; use `dontAsk`.
- **The MCP bridge is a local network/IPC surface.** An HTTP bridge on loopback
  must bind `127.0.0.1`, use an ephemeral port and a per-run bearer token;
  otherwise any local process can invoke pi's tools.
- **No sandboxing story.** See §3.4 item 5.

### 5.5 Terms, rate limits, and whether this is supported

This is the section to read twice.

- **The Agent SDK docs prohibit offering claude.ai login to third parties**:
  *"Unless previously approved, Anthropic does not allow third party developers
  to offer claude.ai login or rate limits for their products, including agents
  built on the Claude Agent SDK. Use the API key authentication methods
  described in the Quickstart instead."*
  ([Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview).) A
  personal tool a user runs against their own login is not obviously "offering
  claude.ai login for a product", but a Swift app shipped to other people that
  drives their subscription plausibly is. This is a product/legal question, not
  an engineering one, and it should be answered before code is written.
- **Subscription usage through `claude -p` is nonetheless real and currently
  sanctioned**: *"Claude Agent SDK, `claude -p`, and third-party app usage still
  draw from your subscription's usage limits"*
  ([support article](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan),
  which also notes a planned separate-monthly-credit scheme that is currently
  **paused** and subject to change). The same article's guidance is that credits
  are for *"individual experimentation and automation"* and that *"teams running
  shared production automation should use Claude Platform with an API key"*.
- **Rate limits are shared and tight.** Claude Code, claude.ai web, desktop and
  mobile all draw from one rolling five-hour pool, plus a weekly cap. This study
  alone drove `rate_limit_info.utilization` from unreported to **0.9** in six
  small calls on a Pro plan — because each call re-pays a multi-thousand-token
  system prompt and an extra haiku call. A pi agent loop making one `stream()`
  call per turn will exhaust a subscription window fast. The
  `rate_limit_event` line makes this observable; it does not make it avoidable.
- **`ANTHROPIC_BASE_URL` / Bedrock / Vertex env vars silently redirect billing.**
  Present in this very host's environment.
- **Branding.** The Agent SDK terms forbid presenting a product as "Claude Code".
  A pi provider surfaced in a Swift app must not be labelled that way.
- **Not a stable interface.** The stream-json shape is documented, but
  `stream_event` being raw Anthropic SSE, the `caller` field on `tool_use`, the
  precedence of OAuth over `ANTHROPIC_API_KEY`, and the `$USER`-keyed keychain
  lookup are all implementation details I observed rather than contracts anyone
  promised.

### 5.6 The Claude Agent SDK as an alternative transport

Assessed and **rejected**. It is Python and TypeScript only — the docs are
explicit: *"The SDK is available as a library for Python and TypeScript only. To
drive the same agent loop from another language, run the CLI as a subprocess
with the `-p` flag and `--output-format json`."* There is no Rust SDK. From Rust
the SDK is strictly worse than the CLI: it spawns the same `claude` binary and
speaks the same stdio protocol, so using it means a Rust → Node/Python →
`claude` chain with an extra runtime dependency, an extra process, and no
additional protocol guarantees. Its genuinely better-specified surfaces —
`canUseTool`, hooks, in-process MCP servers, `setPermissionMode`, `interrupt` —
ride a **control protocol over the same stdio channel** that is not documented
as a public wire format; the CLI's `capabilities: ["interrupt_receipt_v1",
"interrupt_cancel_queued_v1", "msg_lifecycle_v1"]` hints at it, and
`--permission-prompt-tool` exposes a slice of it, but reimplementing it in Rust
against an undocumented framing is a worse bet than the NDJSON surface, which is
documented. **It does not change the auth story at all** — same binary, same
keychain, same OAuth token, same `--bare` caveat.

---

## 6. Recommendation

**Do not build the CLI provider yet. Build the thing that already exists first.**

1. **First, wire up the OAuth path that is already in this workspace.**
   `crates/pi-auth/src/oauth/anthropic.rs` implements the Claude Pro/Max login,
   `AnthropicOAuth::is_subscription()` returns `true`, and
   `crates/pi-provider-anthropic/src/request.rs` already does everything an
   OAuth token needs — `is_oauth_token()`, the mandatory `"You are Claude Code,
   Anthropic's official CLI for Claude."` system block, Claude Code tool-name
   canonicalisation via `to_claude_code_name` / `from_claude_code_name`, and
   `ANTHROPIC_OAUTH_TOKEN` ahead of `ANTHROPIC_API_KEY` in `ANTHROPIC_PROVIDER`.
   `claude setup-token` mints an `sk-ant-oat*` token that drops straight into
   that path.

   This gets subscription billing with **the entire `ApiClient` contract intact**:
   temperature, max_tokens, thinking budgets, cache retention, real tool
   schemas, constrained sampling, deferred responses, headers, retries,
   `on_payload` / `on_response`, honest per-request `Usage`, one HTTP request
   per turn instead of one process, and pi's agent loop unambiguously in charge.
   Everything in §5.1 that the CLI cannot do, this can. It is also already
   written, already tested, and already byte-compatible with upstream.

   The honest counterweight: this path is *less* officially sanctioned than the
   CLI, not more. It uses a subscription OAuth token against `api.anthropic.com`
   with a spoofed Claude Code identity — that is what the `CLAUDE_CODE_VERSION`
   constant and the "Stealth mode" comment in `request.rs` are for. §5.5's terms
   discussion applies to it at least as strongly. If that is unacceptable, the
   right answer is an API key, not a subprocess.

2. **If a CLI provider is still wanted, scope it deliberately narrowly:**
   scope (c) from §4.3 — `--print`, `stream-json`, `--include-partial-messages`,
   `--system-prompt`, `--tools ""`, `--mcp-config` for `Context.tools`,
   `--strict-mcp-config`, `--permission-mode dontAsk`,
   `--no-session-persistence`, env scrubbed to `HOME`/`PATH`/`USER` with all
   `ANTHROPIC_*` removed, and a hard assertion that
   `system/init.apiKeySource == "none"`. Ship it behind a non-default cargo
   feature, document §5 in the crate docs verbatim, and register it as api id
   `claude-cli` so no existing model silently changes transport.

   Before writing the adapter, prototype exactly two things, because both are
   currently unverified and either one can sink the design: **(i)** the
   sentinel-stop in §4.3 — what an MCP `tools/call` response must contain so the
   CLI stops cleanly at the first tool call without the model reacting to a
   fabricated result; **(ii)** whether a signed, non-sandboxed Swift app can
   spawn `claude` and have the keychain read succeed without a user-facing
   dialog. If (ii) fails, nothing else matters.

3. **Do not use the Claude Agent SDK** (§5.6), and **do not use `--bare`**
   (§3.3).

A useful third option, if the goal is broader than billing: keep the existing
`anthropic-messages` adapter for model access, and expose the `claude` CLI
separately as a **pi tool** (a "delegate to Claude Code" tool that pi's loop can
invoke) rather than as an `ApiClient`. That fits what the CLI actually is — an
agent — instead of forcing an agent through a trait that models a single turn.
