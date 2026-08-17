# Spec 10 — Chat (`FormUI/Chat`)

> **Workstream W10.** Owns `app/Sources/FormUI/Chat/`. Satisfies F1, F6, F10's rendering
> half. The single most visible surface — layout details in
> [spec 08 §1](./08-design-system.md) are the acceptance bar.

## 1. Composition

```
ChatView
├── TranscriptView          (ScrollView + LazyVStack, bottom-anchored)
│   ├── UserMessageRow      (bubble, right-aligned, attachments inline)
│   ├── AssistantMessageRow (full width, MarkdownView from W11)
│   ├── ThinkingBlock       (collapsible, shimmer while streaming)
│   ├── ToolCallGroup       (collapsed summary → expanded detail)
│   ├── TurnFooter          ("3m 31s · 5.9k tokens")
│   └── QueuedMessageRow    (F1.7)
└── ComposerView
    ├── ChipRow             (scope, workspace folder, folder picker)
    ├── AttachmentTray      (from W13)
    ├── InputField          (autogrow to 12 lines)
    └── ControlRow          (mode, +, mic, chevron │ model, effort, ContextRing)
```

## 2. Streaming

The hard requirement is that **rendering is driven by deltas, not by re-parsing the
transcript per event**.

- `ChatStore` (W7) accumulates text; `AssistantMessageRow` observes only its own message.
- Markdown re-parse is debounced at ~50 ms *and* forced on block boundaries (a newline
  following a blank line, a fence open/close) so structure appears promptly without
  thrashing. Only the tail block re-renders (W11 provides stable block ids).
- Scroll: pinned to bottom while the user is within 40 pt of the bottom; a "jump to latest"
  pill appears when they scroll away, and auto-scroll never fights the user.
- Scroll offset per session is persisted and restored on route change.

## 3. Message rendering

- **User (F1.2):** right-aligned, `surfaceRaised`, radius `xl`, padding 12/14, max width 72%
  of the column. Attachments render as thumbnail chips above the text.
- **Assistant:** no bubble, full column, `MarkdownView`.
- **Hover actions (F1.5):** copy, retry, branch, and a relative timestamp, revealed on hover
  in the trailing gutter with a `motion.fast` fade. Retry dispatches `retryMessage`; branch
  dispatches `branchFromMessage` and routes to the new session.
- **Errors:** a failed run renders an inline error block with the code, message, and a retry
  button — not a toast.

## 4. Tool calls (F1.3)

- Consecutive tool calls in one turn collapse into a single 28 pt row:
  `Ran 5 commands, used a tool ›`. The verb phrase is derived from tool names and counts
  (`Ran N commands`, `read N files`, `created N files`, `Searching`), matching the
  reference's tone.
- Mutating tools contribute diff counts rendered as `+268` / `-0` in `diffAdd`/`diffRemove`
  with tabular figures.
- Expanded: one row per call with tool name, argument summary, duration, status glyph, and a
  disclosure for the full arguments and result. Results render through `MarkdownView` when
  they are text, and as a file chip when they are a path.
- While running: an indeterminate shimmer, upgraded to a determinate `ProgressBar` when
  `tool_execution_update` carries progress (F6.2).

## 5. Thinking

A collapsible block above the text, labeled with the effort level, collapsed by default once
complete, auto-expanded while streaming. Shimmer treatment distinct from the text caret
(F6.3) — `color.thinking`, lower contrast, italic serif is **not** used here (serif is
wordmark and display only).

## 6. Composer

- Autogrow 1→12 lines then internal scroll (F1.8). `⏎` sends, `⇧⏎` newline, `⌥⏎` newline.
- While streaming, the send button becomes a stop button; `Esc` also aborts (F1.6).
- Sending during a run queues the message (F1.7): it appears as a `QueuedMessageRow` with a
  cancel affordance and is dispatched at the next turn boundary.
- Chips: scope (`Local`), workspace folder basename with full path on hover (F4.2), and a
  folder-picker icon chip opening an `NSOpenPanel` (directories only) that dispatches
  `setWorkspaceRoot` and records a recent root.
- Control row right side: model name and effort opening a searchable picker popover (F8.3),
  and the `ContextRing`.
- Drag-and-drop and paste of files/images land on the composer (F3.1) — the tray itself is
  W13's.

## 7. Context ring (F10)

A 14 pt `ProgressRing` bound to `ContextUsage`. Animates between values (F6.4), recolors at
75% (`warning`) and 90% (`danger`) (F10.2). Click opens a popover: a segment breakdown bar
(system / tools / transcript / attachments / output reserve) with token counts, plus
cumulative session tokens and cost (F10.3). Popover styling per spec 08 §1.

## 8. Empty state (F1.9)

Greeting in `typography.display` (serif) centered above a centered composer, in a 680 pt
column. On first send, the greeting fades out and the composer slides to the bottom over
`motion.emphasized`; the transition must not drop composer focus or typed text.

## 9. Done when

- Acceptance criterion 4 holds: a stub response streams with visible text deltas, a thinking
  block, a collapsed tool group, a turn footer, and a moving context ring — all from core
  events, with **no mock data in Swift**.
- Abort mid-stream leaves a partial message and an `aborted` footer.
- Queue-while-streaming injects at the next turn.
- A 400-line response with tables and code blocks streams without dropped frames (spot-check
  with Instruments; document the result).
- Reduce-motion disables the pulse, shimmer and ring animation.
