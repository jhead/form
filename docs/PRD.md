# form — Product Requirements Document

> **Status:** v1 (MVP scope locked)
> **Owner:** jhead
> **Companion specs:** [`docs/specs/`](./specs) — read [`00-protocol.md`](./specs/00-protocol.md) before writing any code that crosses the Swift/Rust line.

---

## 1. Summary

**form** is a native macOS desktop client for a coding agent. The UI is SwiftUI. Everything
portable — session storage, search, settings, provider catalog, usage analytics, markdown
parsing, context accounting — lives in a Rust core (`form-core`) behind a narrow C ABI, so a
Windows and Linux client can reuse it later without reimplementing app logic.

The agent harness itself is **not** built here. It is being ported in parallel in
[`pi-rs`](../pi-rs) (a Rust port of the [`pi`](https://github.com/earendil-works/pi)
TypeScript SDK). `form` ships against a **stub harness** that emits the same event protocol
`pi-rs` will emit, driven by deterministic mock data. This lets the entire UX be exercised,
demoed and iterated on with no LLM backend and no API keys — while proving the Swift ↔ Rust
path end to end.

### Naming and wordmark

The product is **`form`** — always lowercase, never capitalized, never "Form" at the start of
a sentence. The wordmark is set in a **serif** face (New York, falling back to Charter →
Georgia). Serif is reserved for the wordmark and for display-scale headings (the Home page
greeting); all body and UI text is the system sans face. See
[`08-design-system.md`](./specs/08-design-system.md).

---

## 2. Goals

| # | Goal |
|---|---|
| G1 | Reproduce the reference client's UX with precision — layout, density, motion, interaction grammar — minus product-specific surfaces (mascot, Routines, Dispatch, Customize, plan/billing). |
| G2 | Ship every *generic* coding-agent feature: chat, grouped sessions, attachments with thumbnails, folder confinement, theming, animated indicators, markdown rendering. |
| G3 | Prove the Swift ↔ Rust boundary end to end with a stub harness — real streaming, real event ordering, real persistence, mock content. |
| G4 | Put all portable logic in Rust so Windows/Linux clients are a UI port, not a rewrite. |
| G5 | Multi-provider, multi-model, multi-reasoning-effort model selection with a preferences surface and live context-usage reporting. |
| G6 | A Home page that is a genuinely useful analytics dashboard, not a placeholder. |
| G7 | Browser-grade keyboard control: everything reachable without the mouse. |

## 3. Non-goals (MVP)

- Real LLM requests, real auth/OAuth, real tool execution, real file mutation. The harness is
  a stub; `pi-rs` lands later behind the same protocol.
- Shipping Windows/Linux binaries. The core is *designed* for it; only macOS is built.
- Multi-window / tabs, iCloud or cloud sync, collaboration, plugin or extension APIs.
- App Store sandboxing and notarization. MVP runs unsandboxed and locally signed.
- Terminal emulator pane, git integration, diff editor. (Tool-call *rendering* is in scope;
  tool *execution* is not.)

---

## 4. Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  form.app  (SwiftUI, macOS 14+)                              │
│                                                              │
│   FormUI ── views, animation, layout, menus, shortcuts       │
│   FormDesign ── theme tokens, typography, components         │
│   FormMarkdown ── renders the block tree Rust produces       │
│   FormCore (Swift) ── Codable protocol + actor + AsyncStream │
│   FormFFI ── C module map                                    │
└───────────────────────────┬──────────────────────────────────┘
                            │  C ABI, JSON payloads
┌───────────────────────────┴──────────────────────────────────┐
│  libform_ffi.a  (Rust staticlib)                             │
│                                                              │
│   form-ffi ── 9 extern "C" functions, cbindgen header        │
│   form-core ── domain, store (SQLite), settings, catalog,    │
│                stats engine, markdown, search, stub harness  │
│                                                              │
│   ⟵ later: pi-rs (pi-agent / pi-session / pi-provider-*)     │
└──────────────────────────────────────────────────────────────┘
```

### 4.1 Decision: in-process FFI, not a sidecar

Both were viable. The decision and its escape hatch:

| | In-process C ABI (**chosen**) | Sidecar process + framed IPC |
|---|---|---|
| Latency per event | Function call, ~0 | Write + read + wake, ~50–200 µs |
| Streaming 60 fps token deltas | Trivial | Needs backpressure design |
| Packaging | One binary in the bundle | Two, plus launch/reap/respawn/zombie handling |
| Crash blast radius | Takes the app down | Isolated; app survives |
| Debugging | One process, one debugger | Attach separately, or tail the stream |
| Cross-platform | Same ABI everywhere | Same, plus per-OS process plumbing |
| Future: real tool execution (shell, MCP) | Risky in-process | The right answer |

**Chosen: in-process.** The MVP is a UI project — streaming smoothness, packaging simplicity
and single-process debuggability dominate, and the stub harness executes nothing dangerous.

**The escape hatch is deliberate and cheap.** Because every payload crossing the boundary is
already serialized JSON (never a shared struct, never a pointer into Rust memory), the
transport is swappable. Swift talks to a `CoreTransport` protocol; `FFITransport` is the only
implementation today, and a `SubprocessTransport` speaking the same JSON over a length-prefixed
pipe is an additive change on both sides. When real tool execution lands, sandboxed shell work
moves out of process without the app layer noticing. See [`06-ffi.md`](./specs/06-ffi.md).

### 4.2 The boundary contract

Three verbs, plus lifecycle. Full schema in [`00-protocol.md`](./specs/00-protocol.md).

- **`query`** — synchronous, pure read, returns JSON. Session lists, settings, stats,
  catalog, search. Called from Swift off the main actor.
- **`dispatch`** — asynchronous command, returns an immediate ack containing a
  `commandId`. All effects surface as events. Sending a prompt, aborting, creating or
  renaming or grouping sessions, saving settings.
- **`subscribe`** — Swift registers one callback; Rust invokes it from a dedicated
  dispatcher thread for every event. Swift converts to an `AsyncStream<CoreEvent>` and hops
  to the main actor.

Rationale for "commands are async, results are events": a Swift caller cannot hold or drop a
Rust `Future`, so cancellation is explicit (`AbortSignal`-shaped, matching `pi-core`'s
convention) and every result path is the event stream — one code path for streaming and
non-streaming outcomes alike.

### 4.3 Wire compatibility with `pi-rs`

The transcript types on the boundary — `Message`, content blocks, `ToolCall`, `Usage`,
`StopReason`, `AssistantMessageEvent`, the session `Entry` log — are **structurally identical
to `pi-core`'s**, including `#[serde(rename_all = "camelCase")]` and the `snake_case` event
tags (`text_delta`, `toolcall_end`, …). `form-core` defines them today; when `pi-rs` is
ready, `form-core` deletes its copies and re-exports `pi_core`'s. Any divergence is a bug.

**This is verified, not assumed.** `pi-rs` has completed its port, and
`core/crates/form-core/tests/pi_compat.rs` serializes every transcript type and every
streaming-event variant form produces, reads it back as the `pi-core` type, and fails on any
dropped or altered field — plus the reverse direction, since sessions the real harness writes
must stay readable here. `pi-core` is a dev-dependency only; nothing shipping links it. The
swap plan is [`16-pi-integration.md`](./specs/16-pi-integration.md).

Types that are **form's own** and have no `pi` equivalent — `Session`, `SessionGroup`,
`Workspace`, `Settings`, `UsageStats`, `MarkdownBlock` — live in `form-core::app` and are
free to evolve.

### 4.4 What lives where

| Concern | Rust | Swift |
|---|---|---|
| Session/message persistence, ordering, branching | ✅ | |
| Grouping, pinning, reordering, archive | ✅ | |
| Full-text search over sessions and messages | ✅ | |
| Settings, provider/model catalog, reasoning efforts | ✅ | |
| Usage aggregation, percentiles, streaks, cost | ✅ | |
| Context-window accounting and token estimation | ✅ | |
| Markdown → block tree; code → scoped syntax tokens | ✅ | |
| Path confinement rules for a workspace root | ✅ | |
| Attachment registry, hashing, dedupe, metadata | ✅ | |
| Rendering, layout, animation, scroll | | ✅ |
| Theme tokens → concrete colors; syntax scope → color | | ✅ |
| Image decode + thumbnail raster, pasteboard, drag/drop | | ✅ |
| Windows, menus, keyboard shortcuts, file pickers | | ✅ |

**Note on markdown:** parsing runs in Rust (`pulldown-cmark`) and returns a typed block tree;
syntax highlighting runs in Rust (`syntect`) and returns *scope names with ranges*, never
colors. Swift maps scopes onto the active theme. This keeps one parser for three future
platforms while keeping all color decisions in the design system.

---

## 5. Feature requirements

Each requirement is numbered for traceability; specs and tests reference these IDs.

### F1 — Chat

- **F1.1** Send a message; receive a streamed assistant response with visible token-level
  text deltas, thinking deltas, and tool calls.
- **F1.2** User messages render right-aligned in a filled rounded bubble, max ~72% column
  width. Assistant messages render full-column with no bubble.
- **F1.3** Consecutive tool calls collapse into one summary row — `Ran 5 commands, used a
  tool ›` — expandable to per-call detail. File-mutating calls show `+N -M` diff counts.
- **F1.4** A turn footer shows elapsed wall time and tokens: `3m 31s · 5.9k tokens`.
- **F1.5** Hovering a user message reveals copy / retry / branch actions and a relative
  timestamp.
- **F1.6** Streaming can be interrupted (`Esc`, or the composer's stop button); the run ends
  with `aborted` and the partial message is retained.
- **F1.7** Queue-while-streaming: typing and sending during a run queues the message; it is
  injected at the next turn boundary and shown as queued in the transcript.
- **F1.8** The composer autogrows to 12 lines then scrolls. `⏎` sends, `⇧⏎` newline.
- **F1.9** Empty-session state shows the greeting and the composer centered, matching the
  reference; on first message the layout transitions to the transcript.

### F2 — Sessions and groups

- **F2.1** Sidebar lists sessions newest-first, with a rank number (1–9 map to `⌘1`–`⌘9`).
- **F2.2** Sessions belong to a named group or to `Ungrouped`. Groups collapse, rename, and
  reorder; an empty group shows a `Drag or move sessions here` drop target.
- **F2.3** Drag a session between groups; drop is persisted immediately.
- **F2.4** Session rows show a status dot: idle, streaming (animated), error.
- **F2.5** Rename (inline, `⏎` commits, `Esc` cancels), duplicate, archive, delete with
  confirm. Context menu and keyboard both.
- **F2.6** Titles are auto-derived from the first user message until manually renamed.
- **F2.7** Sidebar is collapsible (`⌘\`); collapsed state persists across launches.

### F3 — Attachments

- **F3.1** Attach via `+` button, drag-and-drop onto the composer or transcript, or paste.
- **F3.2** Image attachments show a thumbnail chip; non-images show a type-glyph chip with
  filename and size.
- **F3.3** Thumbnails are generated once and cached on disk, keyed by content hash.
- **F3.4** Click a thumbnail for a full-size quicklook-style overlay.
- **F3.5** Attachments are removable pre-send and render inline in the sent user message.
- **F3.6** Oversized files (>10 MB) and unsupported types are rejected with an inline reason.

### F4 — Folder confinement

- **F4.1** Each session has an optional workspace root, chosen from the composer's folder
  chip or preferences.
- **F4.2** The chip shows the folder's basename (`dev`) with the full path on hover.
- **F4.3** Confinement is enforced in Rust: a path resolution API rejects escapes via `..`,
  symlinks, and absolute paths outside the root. This is the API real tools will call.
- **F4.4** Recently used roots are offered in a picker.
- **F4.5** A session with no root is explicitly "unconfined" and labeled as such.

### F5 — Theming

- **F5.1** Light, Dark, and System modes; System follows the OS live.
- **F5.2** All color, spacing, radius, typography and motion values come from a token set —
  no literal colors in view code. Enforced by review and a lint test.
- **F5.3** Themes are data (`Theme` struct + JSON), so alternates can be added without touching
  views.
- **F5.4** Switching modes animates the crossfade and does not drop scroll position or focus.

### F6 — Animated indicators

- **F6.1** Streaming pulse on the active session row and in the transcript.
- **F6.2** Tool-call rows animate from running → complete, with a determinate progress bar
  where the stub reports progress.
- **F6.3** A "thinking" shimmer distinct from the text-streaming caret.
- **F6.4** Context-usage ring animates between values rather than snapping.
- **F6.5** All motion respects `NSWorkspace.accessibilityDisplayShouldReduceMotion`.

### F7 — Markdown

- **F7.1** Headings, bold/italic/strike, inline code, links, ordered/unordered/task lists,
  blockquotes, tables, horizontal rules, images, footnotes.
- **F7.2** Fenced code blocks with language label, syntax highlighting, copy button, and
  horizontal scroll — never wrapping the page.
- **F7.3** Renders incrementally during streaming without reflow flicker; unterminated fences
  and half-written tables degrade gracefully.
- **F7.4** Text is selectable across blocks; copy yields the original markdown source.
- **F7.5** Links open in the default browser; `file://` links reveal in Finder.

### F8 — Providers, models, reasoning effort

- **F8.1** Catalog covers Anthropic, OpenAI, Google, OpenRouter, xAI, Groq, Mistral, DeepSeek,
  and a local/Ollama entry, each with a model list carrying context window, max output,
  pricing, and capability flags (vision, tools, reasoning, caching).
- **F8.2** Reasoning effort follows `pi`'s ladder: `off, minimal, low, medium, high, xhigh,
  max`, filtered per model.
- **F8.3** Model picker in the composer bar shows model + effort, matching the reference
  (`Opus 5   High`), with a searchable popover.
- **F8.4** Per-session model override; a global default in preferences.
- **F8.5** API keys are entered in preferences, stored in the macOS Keychain, and never
  logged. The stub harness ignores them; the surface must be real.

### F9 — Preferences

- **F9.1** A modal sheet (`⌘,`) with tabs: General, Providers, Models, Appearance, Editor,
  Shortcuts, Advanced.
- **F9.2** Changes persist through the core and take effect without restart.
- **F9.3** Import/export settings as JSON; reset-to-defaults with confirm.

### F10 — Context usage

- **F10.1** A ring in the composer bar shows used / total context for the session's model,
  with a popover breakdown by segment (system, tools, transcript, attachments, output
  reserve).
- **F10.2** Thresholds recolor at 75% and 90%.
- **F10.3** The popover also shows cumulative session tokens and cost.
- **F10.4** Accounting is computed in Rust from the real transcript, not faked in the view.

### F11 — Home analytics

The Home page is the app's dashboard. Period selector: `7d / 30d / All`, plus tabs
`Overview / Models / Activity / Cost`.

- **F11.1 Headline tiles** — sessions, messages, total tokens, active days, current streak,
  longest streak, peak hour, favorite model.
- **F11.2 Activity heatmap** — GitHub-style day × week grid, intensity by tokens, with
  hover detail.
- **F11.3 Time series** — tokens and messages per day (stacked area, input / output /
  cache-read / cache-write), and sessions per day.
- **F11.4 Hour-of-day histogram** and a day-of-week × hour matrix.
- **F11.5 Model breakdown** — share of tokens and of messages by model (donut + ranked bars),
  with per-model cost.
- **F11.6 Latency and throughput** — per-model time-to-first-token and output tokens/sec, as
  p50 / p90 / p99 bars plus a distribution plot.
- **F11.7 Cost** — spend over time, by provider, by model, and a projected monthly run rate.
- **F11.8 Tool usage** — most-invoked tools, success rate, mean duration.
- **F11.9 Session leaderboard** — longest sessions by tokens, by duration, by turn count,
  each linking to the session.
- **F11.10 Cache effectiveness** — cache-read vs cache-write ratio over time and estimated
  savings.
- **F11.11** All charts use Swift Charts, share axis/tooltip/legend styling, and read from a
  single `stats.query` result — no per-chart round trips.
- **F11.12** Empty and sparse states are designed, not blank.

### F12 — Keyboard shortcuts

Browser-grade. Full table in [`14-shortcuts-commands.md`](./specs/14-shortcuts-commands.md).

| Keys | Action |
|---|---|
| `⌘N` | New chat |
| `⌘⇧N` | New chat in current group |
| `⌘[` / `⌘]` | Previous / next session |
| `⌘⌥←` / `⌘⌥→` | Same, alternate binding |
| `⌘1`…`⌘9` | Jump to session by rank |
| `⌘K` | Command palette / session search |
| `⌘F` | Find in current session |
| `⌘G` / `⌘⇧G` | Find next / previous |
| `⌘\` | Toggle sidebar |
| `⌘,` | Preferences |
| `⌘⇧H` | Home |
| `⌘W` | Close (archive) session |
| `⌘⇧K` | Clear / new from current |
| `⌘↩` | Send (when composer unfocused) |
| `Esc` | Stop streaming / dismiss overlay |
| `⌘⇧C` | Copy last response |
| `⌘⇧D` | Toggle appearance |
| `⌘⇧F` | Choose workspace folder |
| `⌘0` | Reset text size · `⌘+` / `⌘-` zoom |

- **F12.1** Every shortcut appears in the app menus with its key equivalent.
- **F12.2** A discoverable cheat-sheet overlay (`⌘/`).
- **F12.3** Shortcuts are declared in one table consumed by both menus and handlers — no
  duplicated key definitions.

### F13 — Search

- **F13.1** `⌘K` searches session titles and message bodies, ranked, with snippets and match
  highlighting; arrow keys + `⏎` to open.
- **F13.2** `⌘F` searches within the open session, highlights all matches, scrolls to and
  focuses each in turn, and shows `n of m`.
- **F13.3** Both are backed by SQLite FTS5 in the core.

---

## 6. Screen inventory

| Screen | Spec |
|---|---|
| App shell — sidebar, content, toolbar, empty states | [`09-app-shell-sidebar.md`](./specs/09-app-shell-sidebar.md) |
| Sidebar — groups, session rows, drag/drop, footer | [`09-app-shell-sidebar.md`](./specs/09-app-shell-sidebar.md) |
| Chat — transcript, composer, tool calls, context ring | [`10-chat.md`](./specs/10-chat.md) |
| Home — analytics dashboard | [`12-home-analytics.md`](./specs/12-home-analytics.md) |
| Preferences — modal, 7 tabs | [`13-preferences-attachments.md`](./specs/13-preferences-attachments.md) |
| Command palette, find bar, shortcut cheat-sheet | [`14-shortcuts-commands.md`](./specs/14-shortcuts-commands.md) |

---

## 7. Workstreams

Wave 1 runs in parallel after the shared contract and build system are in place (done up
front, by the orchestrator, not by a workstream).

| ID | Workstream | Owns | Depends on |
|---|---|---|---|
| **W0** | Contract + scaffold + e2e proof | protocol types, `Package.swift`, Cargo workspace, build scripts | — |
| **W1** | Core domain + store | `form-core::app`, SQLite schema, sessions, groups, search, workspace | W0 |
| **W2** | Stub harness | `form-core::harness`, event generation, mock content, timing | W0 |
| **W3** | Stats engine | `form-core::stats`, aggregation, percentiles, streaks | W0 |
| **W4** | Catalog + settings | `form-core::catalog`, `::settings`, providers, models, context accounting | W0 |
| **W5** | Markdown + highlighting | `form-core::markdown` | W0 |
| **W6** | FFI + CLI | `form-ffi`, `form-cli`, header generation, e2e tests | W0 |
| **W7** | Swift core client | `FormCore` — transport, Codables, actor, stores | W0 |
| **W8** | Design system | `FormDesign` — tokens, themes, typography, primitives | W0 |
| **W9** | App shell + sidebar | `FormUI/Shell`, `FormUI/Sidebar` | W7, W8 |
| **W10** | Chat | `FormUI/Chat` — transcript, composer, tool rows, indicators | W7, W8 |
| **W11** | Markdown rendering | `FormMarkdown` | W8 |
| **W12** | Home analytics | `FormUI/Home` | W7, W8 |
| **W13** | Preferences + attachments | `FormUI/Preferences`, `FormUI/Attachments` | W7, W8 |
| **W14** | Shortcuts, palette, find | `FormUI/Commands` | W9 |

Directory ownership is exclusive — no two workstreams edit the same file. Cross-cutting
files (`Package.swift`, `Cargo.toml`, protocol types) are frozen after W0; a workstream that
needs a change requests it rather than making it.

---

## 8. Acceptance criteria

The MVP is done when, on a clean machine:

1. `make` builds the Rust core, the Swift package, and assembles `form.app`, with zero
   warnings in Rust (`clippy -D warnings`) and no Swift errors.
2. `make test` passes: Rust unit tests, the `form-cli` end-to-end protocol test, and Swift
   tests for the core client, markdown rendering, and shortcut table integrity.
3. Launching `form.app` shows the Home dashboard populated from seeded mock data, with every
   chart in F11 rendering real aggregates from the core.
4. `⌘N` opens a new chat; sending a message streams a stub response with visible text deltas,
   a thinking block, at least one collapsed tool-call group, a turn footer, and a context ring
   that moves — all sourced from Rust events, with no mock data in Swift.
5. Quitting and relaunching restores sessions, groups, scroll position, sidebar state,
   selected model and theme.
6. Every shortcut in F12 works and is present in the menu bar.
7. Toggling appearance repaints every surface with no hardcoded color leaking through.
8. Dragging a session between groups persists across relaunch.
9. Attaching an image shows a thumbnail, sends, and renders inline in the transcript.
10. A workspace root can be chosen and a path-confinement unit test proves escapes are
    rejected.

---

## 9. Risks

| Risk | Mitigation |
|---|---|
| Rust callbacks into Swift from a non-main thread cause data races | One dispatcher thread in Rust; Swift bridges to `AsyncStream` and hops to `@MainActor` at a single point (W7 owns it). |
| Parallel workstreams diverge on the protocol | Protocol types are written once in W0, in both languages, and frozen. A round-trip test in W6 fails if they drift. |
| Streaming markdown re-parse costs frames | Parse is debounced and incremental; only the tail block re-renders. Budget: 16 ms at 120 blocks. |
| `pi-rs` lands with a different shape than assumed | **Retired.** `pi-rs` is ported and `tests/pi_compat.rs` proves the wire formats agree in both directions. |
| Real tool execution makes in-process risky | The `CoreTransport` seam keeps the sidecar option open; spec 16 §3 says to revisit the §4.1 decision when `pi-tools` runs real commands, rather than inheriting it. |
| Xcode project drift across agents | No `.xcodeproj` in source control — SwiftPM is the build, `xcodegen` generates a project on demand. |
