# Spec 11 — Markdown rendering (`FormMarkdown`)

> **Workstream W11.** Owns `app/Sources/FormMarkdown/`. Renders the `MarkdownDoc` block tree
> produced by [spec 05](./05-markdown-core.md). Satisfies F7. Parses nothing — if you find
> yourself writing a parser, the block tree is missing something; ask.

## 1. API

```swift
public struct MarkdownView: View {
    public init(doc: MarkdownDoc, style: MarkdownStyle = .default)
}
```

`MarkdownStyle` is derived from `Theme` (spec 08) — sizes, spacing, code font, list indents.
No color or font literals in this module.

## 2. Rendering rules

- **Blocks** render as a `VStack` over `ForEach(doc.blocks, id: \.id)`. Stable ids from the
  core keep identity across streaming re-parses (spec 05 §2) — never index-based `ForEach`.
- **Spans** compose into a single `AttributedString` per paragraph so text shapes and wraps
  as one run. Inline code gets a background chip via `AttributedString` background + a small
  inset.
- **Code blocks (F7.2):** language label in the top-left of a `surfaceRaised` panel, a copy
  button top-right (appears on hover, confirms with a checkmark for 1.2 s), horizontal
  `ScrollView` — the block scrolls, the page never does (this is a hard requirement).
  Optional line numbers per `editor.showLineNumbers`, and soft wrap per `editor.wrapCode`.
  Highlighting applies `CodeToken` UTF-16 ranges onto the `AttributedString` using the
  theme's `SyntaxTokens` longest-prefix scope match.
- **Tables:** a `Grid` with header emphasis, column alignment from `ColumnAlign`, zebra rows
  at 3% surface tint, and horizontal scroll when wider than the column.
- **Lists:** correct nesting indents, markers by depth (`•`, `◦`, `▪`), ordered lists respect
  `start`, task lists render non-interactive checkboxes reflecting `checked`.
- **Quotes:** 2 pt leading rule in `border`, 12 pt inset, secondary text.
- **Images:** `AsyncImage` for remote, direct load for `file://` and attachment refs, with a
  max height of 400 pt, rounded corners, and a placeholder that reserves space so streaming
  does not reflow.
- **Links (F7.5):** `http(s)` and `mailto` open via `NSWorkspace`; `file://` reveals in
  Finder. Underline on hover, `accent` color, tooltip shows the URL.
- **Html blocks:** rendered as escaped monospace text, never interpreted.

## 3. Selection and copy (F7.4)

Text must be selectable across blocks. SwiftUI's `.textSelection(.enabled)` does not span
separate `Text` views, so implement a `MarkdownTextView` backed by a single `NSTextView`
(non-editable, `drawsBackground = false`, link handling delegated) per contiguous run of
text blocks, and interleave native SwiftUI views for code blocks, tables and images.
`⌘C` on a selection yields the **original markdown source** for the selected range — keep a
source-range map from the block tree to make this exact.

Document the tradeoff in a comment: full-document selection across native subviews is not
achievable without a custom text system; contiguous-run selection plus "copy message" (from
the hover actions in W10) covers the real use.

## 4. Streaming behaviour (F7.3)

- The last block may have `partial: true`: suppress its copy button and trailing chrome, and
  render an unterminated fence as a code block already, not as paragraph text.
- Re-render only blocks whose id changed. Measure this — a `ForEach` that rebuilds every
  block per token will show up immediately as dropped frames on a long response.
- Never animate block insertion during streaming; the caret moving is the motion.

## 5. Done when

- A fixture document exercising every block and span type renders correctly in both themes,
  with a snapshot test.
- Streaming a 60 KB document token-by-token keeps frame time under budget (measure and
  record).
- Long code lines scroll inside the block; the transcript column never scrolls horizontally.
- Copying a selection returns markdown source, not rendered text.
- `javascript:` links from the core arrive as plain text and are not clickable.
