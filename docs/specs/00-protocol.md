# Spec 00 — The boundary protocol

> **Frozen after W0.** Every other workstream reads this. If you need a change, ask the
> orchestrator; do not edit `protocol.rs` or `Protocol.swift` unilaterally.

The Swift app and the Rust core exchange **JSON only**. No shared structs, no pointers into
Rust memory, no lifetimes. This is what makes the transport swappable (in-process FFI today,
subprocess later) and what makes the core reusable from a future Windows/Linux client.

## 1. Encoding rules

1. All JSON keys are `camelCase` (`#[serde(rename_all = "camelCase")]` in Rust,
   `CodingKeys` or a `.convertFromSnakeCase`-free explicit mapping in Swift).
2. Sum types are **internally tagged on `type`**. Command and query tags are `camelCase`
   (`sendPrompt`); `AssistantMessageEvent` tags are `snake_case` (`text_delta`,
   `toolcall_end`) — **this is inherited from `pi` and must not be normalized**.
3. Timestamps are Unix milliseconds, `i64`, named `timestamp` / `createdAt` / `updatedAt`.
4. Ids are opaque strings. Rust mints them; Swift never constructs one.
5. Absent optional fields are omitted, not `null` (`skip_serializing_if = "Option::is_none"`).
6. Unknown fields are ignored on both sides — additive changes are non-breaking.

## 2. Lifecycle

```c
uint32_t     form_abi_version(void);
FormCore*    form_core_new(const char *config_json, char **err_out);
void         form_core_free(FormCore*);
int32_t      form_core_subscribe(FormCore*, FormEventCallback cb, void *ctx);
void         form_core_unsubscribe(FormCore*, int32_t token);
char*        form_core_query(FormCore*, const char *query_json);
char*        form_core_dispatch(FormCore*, const char *command_json);
void         form_string_free(char*);
const char*  form_last_error(void);
```

`form_abi_version()` returns `FORM_ABI_VERSION`; Swift asserts a match at startup and
refuses to run against a mismatched core. Bump on any breaking change to this file.

`config_json` is `CoreConfig`:

```jsonc
{
  "dataDir": "/Users/x/Library/Application Support/form",
  "seedMockData": true,     // populate the store with demo sessions + usage history
  "logLevel": "info"
}
```

## 3. Queries — synchronous reads

Request: `{"type": "<tag>", ...}`. Response: `{"ok": true, "data": …}` or
`{"ok": false, "error": {"code": "...", "message": "...", "detail": …}}`.

| Tag | Params | Returns |
|---|---|---|
| `listSessions` | `includeArchived?` | `{ groups: SessionGroup[], sessions: SessionSummary[] }` |
| `getSession` | `sessionId` | `Session` (with full `entries`) |
| `searchSessions` | `q`, `limit?` | `SearchHit[]` |
| `searchInSession` | `sessionId`, `q` | `SearchHit[]` |
| `getSettings` | — | `Settings` |
| `getCatalog` | — | `{ providers: Provider[] }` |
| `getStats` | `range` (`d7`\|`d30`\|`all`), `tz` | `UsageStats` |
| `getContextUsage` | `sessionId` | `ContextUsage` |
| `renderMarkdown` | `text`, `theme?` | `MarkdownDoc` |
| `resolvePath` | `sessionId`, `path` | `{ resolved, insideRoot }` |
| `getAttachment` | `attachmentId` | `Attachment` |
| `listRecentRoots` | — | `Workspace[]` |

Queries must be cheap and must never block on I/O longer than a frame. Anything expensive is
a command.

## 4. Commands — asynchronous effects

Request: `{"type": "<tag>", ...}`. Immediate response: `{"ok": true, "data": {"commandId": "cmd_…"}}`.
All outcomes arrive as events carrying the same `commandId`.

| Tag | Params |
|---|---|
| `createSession` | `groupId?`, `title?`, `workspaceRoot?`, `modelRef?` |
| `sendPrompt` | `sessionId`, `text`, `attachmentIds?` |
| `abortRun` | `sessionId` |
| `renameSession` | `sessionId`, `title` |
| `deleteSession` | `sessionId` |
| `archiveSession` | `sessionId`, `archived` |
| `moveSession` | `sessionId`, `groupId?`, `index` |
| `createGroup` / `renameGroup` / `deleteGroup` / `reorderGroup` | … |
| `setSessionModel` | `sessionId`, `modelRef`, `thinkingLevel` |
| `setWorkspaceRoot` | `sessionId`, `path?` |
| `updateSettings` | `settings` (whole document) |
| `addAttachment` | `sessionId`, `path` \| `bytesBase64`, `filename`, `mime` |
| `removeAttachment` | `attachmentId` |
| `branchFromMessage` | `sessionId`, `entryId` |
| `retryMessage` | `sessionId`, `entryId` |

## 5. Events — the outbound stream

Every event: `{"type": "<tag>", "timestamp": 1755…, "commandId"?: "cmd_…", ...}`.

### 5.1 Run lifecycle (mirrors `pi`'s `AgentEvent`)

| Tag | Payload |
|---|---|
| `run_start` | `sessionId`, `runId` |
| `turn_start` | `sessionId`, `runId` |
| `message_start` | `sessionId`, `entry: Entry` |
| `message_update` | `sessionId`, `entryId`, `event: AssistantMessageEvent` |
| `message_end` | `sessionId`, `entry: Entry` |
| `tool_execution_start` | `sessionId`, `toolCallId`, `toolName`, `args` |
| `tool_execution_update` | `sessionId`, `toolCallId`, `partialResult` |
| `tool_execution_end` | `sessionId`, `toolCallId`, `result`, `isError` |
| `turn_end` | `sessionId`, `runId`, `usage: Usage` |
| `run_end` | `sessionId`, `runId`, `outcome`: `completed`\|`aborted`\|`failed`, `usage`, `durationMs` |

**Ordering contract**, identical to `pi-core`'s: `run_start` first; exactly one terminal
`run_end`; `message_update` only between the matching `message_start`/`message_end`;
provider and runtime failures are *encoded in the stream*, never returned as an error from
`dispatch`.

### 5.2 Store and app events

| Tag | Payload |
|---|---|
| `session_created` / `session_updated` / `session_deleted` | `session` / `sessionId` |
| `groups_changed` | `groups: SessionGroup[]` |
| `settings_changed` | `settings: Settings` |
| `context_usage` | `sessionId`, `usage: ContextUsage` |
| `stats_invalidated` | — (Swift re-queries on its own schedule) |
| `attachment_added` / `attachment_removed` | `attachment` / `attachmentId` |
| `error` | `code`, `message`, `detail?` — non-fatal, surfaced as a toast |

### 5.3 `AssistantMessageEvent`

Copied verbatim from `pi-core::event`. Tags are `snake_case`, with `toolcall_start`,
`toolcall_delta`, `toolcall_end` spelled exactly that way. Non-terminal events carry
`partial: AssistantMessage`; terminal events are `done { reason, message }` and
`error { reason, error }`.

Swift renders from the deltas and reconciles against `partial` — never re-parses the whole
transcript per event.

## 6. Domain types

`Message`, `AssistantMessage`, content blocks (`text`, `thinking`, `image`, `toolCall`),
`ToolCall`, `ToolResult`, `Usage`, `Cost`, `StopReason`, `Entry` — **structurally identical
to `pi-core`**. See [`pi-rs/crates/pi-core/src/`](../../../pi-rs/crates/pi-core/src/).

form-specific:

```rust
struct SessionSummary { id, title, groupId: Option<String>, index: u32,
                        workspaceRoot: Option<String>, modelRef: ModelRef,
                        status: SessionStatus, messageCount, totalTokens,
                        createdAt, updatedAt, archived, pinned }
struct SessionGroup   { id, name, index, collapsed }
enum   SessionStatus  { idle, streaming, error }
struct ModelRef       { providerId, modelId, thinkingLevel }
struct ContextUsage   { used, total, segments: [{ kind, tokens }], cost: Cost }
struct MarkdownDoc    { blocks: [MarkdownBlock] }   // see spec 05
struct UsageStats     { … }                          // see spec 03
struct Settings       { … }                          // see spec 04
```

## 7. Threading

- `query` runs on the calling thread and must be re-entrant and lock-light.
- `dispatch` enqueues onto the core's tokio runtime and returns immediately.
- Events are delivered on **one dedicated dispatcher thread**, in order, never concurrently.
  Swift's bridge is the single place that hops to `@MainActor`.
- The callback must not re-enter the core. Swift's bridge only appends to an
  `AsyncStream.Continuation`.

## 8. Compatibility test

`form-cli protocol-dump` writes one instance of every command, query and event to
`core/tests/fixtures/protocol/*.json`. The Swift test target decodes all of them and
re-encodes; a diff fails the build. This is the tripwire that catches Swift/Rust drift.
