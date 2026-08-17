# Spec 07 — Swift core client (`FormCore`)

> **Workstream W7.** Owns `app/Sources/FormCore/` and `app/Sources/FormFFI/`. This is the
> only module that touches C. Everything above it sees Swift types and `@Observable` stores.

## 1. Layers

```
FormFFI          — module map exposing core/include/form.h + libform_ffi.a
  ↓
CoreTransport    — protocol: query / dispatch / events
  FFITransport   — the one implementation today
  MockTransport  — in-memory, for SwiftUI previews and tests
  ↓
CoreClient       — actor; typed query/dispatch, AsyncStream<CoreEvent>
  ↓
Stores           — @Observable @MainActor: SessionStore, ChatStore, SettingsStore,
                   StatsStore, CatalogStore
```

The `CoreTransport` indirection is what keeps the sidecar option open (PRD §4.1). Nothing
above `CoreClient` may reference `FormFFI`.

## 2. Protocol types

Hand-written `Codable` mirrors of spec 00 — commands, queries, events, domain types.
Requirements:

- Enums with associated values decode via a `type` discriminator. Write the
  `init(from:)`/`encode(to:)` explicitly; do not rely on a library.
- `AssistantMessageEvent` uses `snake_case` tags (`text_delta`, `toolcall_end`) while
  commands use `camelCase` — do **not** apply a global key strategy; use explicit
  `CodingKeys`.
- Unknown event and block types decode to an `.unknown(type: String, raw: JSONValue)` case
  rather than throwing. A core that is newer than the app must not crash it.
- All types are `Sendable`, `Equatable`, and `Identifiable` where they have an `id`.

The protocol-fixture test (spec 06 §3) lives in this workstream's test target and must pass
before anything else is called done.

## 3. `CoreClient`

```swift
public actor CoreClient {
    public init(config: CoreConfig, transport: CoreTransport) throws
    public func query<Q: CoreQuery>(_ q: Q) async throws -> Q.Response
    @discardableResult public func dispatch(_ c: CoreCommand) async throws -> CommandID
    public nonisolated var events: AsyncStream<CoreEvent> { get }
}
```

- ABI version is asserted in `init`; a mismatch throws a diagnosable error.
- `query` is typed by an associated `Response` so call sites never cast.
- The C callback context is an `Unmanaged<CallbackBox>` passed as `void*`; the box holds the
  `AsyncStream.Continuation`. **The callback body does nothing but `yield`** — no
  allocation-heavy work, no re-entry into the core, no main-thread hop (spec 00 §7).
- Deinit unsubscribes before freeing the handle, and the box is released only after
  `unsubscribe` returns.
- Buffering policy is `.bufferingNewest(4096)` with a dropped-event counter surfaced as a
  diagnostic — a stalled consumer must degrade visibly, not silently.

## 4. Stores

`@MainActor @Observable` classes that own UI-facing state. One event-pump task per store, or
one shared pump that fans out — pick one and document it.

- **`SessionStore`** — groups, session summaries, selection, ordering. Applies
  `session_created/updated/deleted` and `groups_changed`. Optimistic local reorder on drag,
  reconciled against the next event.
- **`ChatStore`** — per-session transcript. Applies `message_start/update/end`,
  `tool_execution_*`, `turn_end`, `run_end`. **Reconciliation rule:** apply deltas
  incrementally for rendering; when a `partial` arrives, assert-and-repair against it in
  debug and repair silently in release. Never re-render the whole transcript per event.
- **`SettingsStore`** — the settings document, plus Keychain-backed API keys (the keys never
  cross the FFI boundary; the store writes `hasKey` through `updateSettings`).
- **`StatsStore`** — caches `UsageStats` per `(range, tz)`, refetches on
  `stats_invalidated`, coalesced to at most one fetch per 2 s.
- **`CatalogStore`** — providers and models, loaded once.

## 5. Keychain

`KeychainStore` wraps `SecItem*` with service `dev.jhead.form`, account = provider id.
Read/write/delete, no logging of values, errors surfaced as typed cases. Unit-tested against
a temporary keychain or skipped cleanly when unavailable in CI.

## 6. Done when

- Protocol fixtures round-trip (spec 06 §3).
- A test creates a real `CoreClient` against a temp `dataDir`, dispatches `sendPrompt`, and
  asserts the full event sequence arrives in order and the reconstructed transcript equals
  the terminal `partial`.
- Free-while-streaming does not crash or hang (mirrors the Rust-side test from Swift).
- `MockTransport` replays a recorded event log so every SwiftUI preview works with no Rust
  build.
