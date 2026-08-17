# Spec 08 — Design system (`FormDesign`)

> **Workstream W8.** Owns `app/Sources/FormDesign/`. Nothing else may define a color, font,
> radius, duration or spacing literal. Every other UI workstream consumes this module.

## 1. Reference UI — written description

The implementer cannot see the reference screenshots, so this is the specification.

**Window.** Single window, unified title bar, no visible toolbar chrome. Traffic lights sit
over the sidebar. Window corner radius is the system default; the sidebar and content share
one continuous surface separated by a hairline, not a shadow.

**Sidebar** (leading, resizable 220–420 pt, default 300 pt, `.ultraThin` material over the
window background — subtly lighter than the content pane in light mode, subtly darker in
dark mode):

- Row 1: traffic lights, then two icon buttons at the trailing edge of that row — a sidebar
  toggle glyph and a magnifier. ~28 pt tall, icons 15 pt, secondary color.
- Row 2: a **segmented control**, full width minus 16 pt inset, two segments: `Home` (house
  glyph) and `Code` (angle-bracket glyph). Selected segment gets a raised white/elevated
  chip with a soft shadow; unselected is plain. 34 pt tall, 8 pt radius.
- A nav list of full-width rows, 34 pt tall, 8 pt radius on hover, 12 pt leading inset,
  icon (15 pt) + label (13 pt) with a 10 pt gap. In `form` the list is exactly one row:
  `+ New`. (The reference has more rows; they are product-specific and dropped.)
- **Group sections.** A section header is 11 pt, `secondary`, medium weight, 24 pt tall,
  with a disclosure chevron that appears on hover and a trailing icon button for group
  actions. An empty group shows one 32 pt row of 11 pt tertiary italic text:
  `Drag or move sessions here`.
- **Session rows.** 32 pt tall, 8 pt radius. Leading: a 16 pt-wide slot holding either the
  rank number (11 pt, tertiary, tabular) or, on hover/selection, a status dot (6 pt). Then
  the title, 13 pt, single line, truncating tail. Selected row: fill at
  `color.surfaceSelected`, title at `color.textPrimary`. Unselected: title at
  `color.textSecondary`.
- Footer, pinned: a 24 pt circular monogram avatar, then a 13 pt name, a `·` separator and a
  12 pt tertiary label, and a trailing chevron opening a menu.

**Content pane.**

- *Empty / home-like state:* content is centered in a column of max width 680 pt. A display
  greeting sits at the top (28 pt), and the composer is anchored at the bottom.
- *Chat state:* a header row, 44 pt tall, with the session title (14 pt medium) and a small
  workspace chip beside it; the trailing edge carries three-to-four 15 pt icon buttons and a
  `⋮` overflow.
- The transcript scrolls in a column of max width 720 pt, centered, with 24 pt horizontal
  padding.

**Composer.** Pinned to the bottom of the content column, 680–720 pt wide.

- Above the field: a row of small chips, 24 pt tall, 6 pt radius, 11 pt label, hairline
  border, e.g. a scope chip (`Local`), a folder chip (`dev`) and an icon-only chip that
  opens a folder picker.
- The field: 1 pt border, 12 pt radius, 12 pt inner padding, 14 pt text, placeholder at
  tertiary. A `⏎` glyph sits at the trailing inner edge. Focus raises the border to
  `color.borderFocus` and adds a 3 pt soft focus ring.
- Below the field: a left cluster — a mode label (`Auto`), a `+` button, a mic button, a
  chevron — and a right cluster — model name (`Opus 5`), effort (`High`), and a 14 pt
  context-usage ring. All 12 pt, secondary.

**Message rendering.** User messages: right-aligned, `color.surfaceRaised` fill, 12 pt
radius, 12/14 pt padding, max width 72% of the column. Assistant messages: no bubble, full
column width, 14 pt text, 1.55 line height. Tool-call groups: a single 28 pt row, 13 pt
text, secondary, with a trailing `›` chevron that rotates 90° when expanded; diff counts
render as `+268` in `color.diffAdd` and `-0` in `color.diffRemove`, tabular figures. A turn
footer line is 11 pt tertiary: `3m 31s · 5.9k tokens`, preceded by a small glyph.

**Popovers.** 10 pt radius, 1 pt hairline border, strong shadow, 12 pt padding, `.regular`
material background. A row is a label at 12 pt secondary on the leading edge and a value at
12 pt primary on the trailing edge; progress bars are 3 pt tall, fully rounded, on a 10%
track.

## 2. Tokens

Tokens are a `Theme` value; views read them from `@Environment(\.theme)`. **No view may
construct a `Color`, `Font`, or raw number that belongs here.**

```swift
public struct Theme: Sendable, Equatable, Codable {
    public var id: String            // "light", "dark"
    public var color: ColorTokens
    public var typography: TypeTokens
    public var metrics: MetricTokens
    public var motion: MotionTokens
    public var syntax: SyntaxTokens
}
```

### 2.1 Color tokens

Semantic only — no `blue500`. Both themes must define every key.

`background`, `backgroundSidebar`, `surface`, `surfaceRaised`, `surfaceSelected`,
`surfaceHover`, `overlay`, `border`, `borderStrong`, `borderFocus`, `textPrimary`,
`textSecondary`, `textTertiary`, `textInverted`, `accent`, `accentHover`, `accentMuted`,
`success`, `warning`, `danger`, `info`, `diffAdd`, `diffRemove`, `streaming`, `thinking`,
`chartSeries` (ordered array of 8), `chartGrid`, `chartAxis`, `heatmapScale` (5 stops).

Light theme anchors: `background` #FFFFFF, `backgroundSidebar` #FAFAF9, `surfaceRaised`
#F2F1EF, `border` #E6E4E0, `textPrimary` #1A1917, `textSecondary` #6B6862,
`textTertiary` #9A968E, `accent` #C15F3C.
Dark theme anchors: `background` #1A1917, `backgroundSidebar` #131211, `surfaceRaised`
#262421, `border` #33302C, `textPrimary` #F5F3EF, `textSecondary` #A8A39B,
`textTertiary` #736E66, `accent` #D97757.

Contrast: every text-on-surface pair must meet WCAG AA (4.5:1 body, 3:1 for ≥18 pt). A unit
test asserts this over the full token matrix for both themes.

### 2.2 Typography

Two families:

- **Serif** — `New York` via `.system(size:design: .serif)`, fallback Charter → Georgia.
  Used *only* for: the `form` wordmark, and the Home greeting/display headings.
- **Sans** — the system face (SF Pro) for all UI and body text.
- **Mono** — `SF Mono`, fallback Menlo, for code, token counts, and tabular figures.

| Token | Size / weight / family |
|---|---|
| `wordmark` | 20 / regular / serif |
| `display` | 28 / regular / serif |
| `title` | 17 / semibold / sans |
| `heading` | 15 / semibold / sans |
| `body` | 14 / regular / sans, line height 1.55 |
| `bodyStrong` | 14 / semibold / sans |
| `ui` | 13 / regular / sans |
| `uiMedium` | 13 / medium / sans |
| `caption` | 12 / regular / sans |
| `micro` | 11 / regular / sans |
| `code` | 12.5 / regular / mono, line height 1.5 |
| `codeInline` | 13 / regular / mono |

All sizes scale with a user text-size multiplier (`⌘+` / `⌘-` / `⌘0`, 0.85–1.4).

### 2.3 Metrics

`spacing` scale: 2, 4, 6, 8, 12, 16, 20, 24, 32, 40, 48 (`.xxs … .xxxl`).
`radius`: 4 (`sm`), 6 (`md`), 8 (`lg`), 12 (`xl`), 999 (`pill`).
`hairline` = 1 / `NSScreen.backingScaleFactor`.
Named metrics: `sidebarWidth` 300 (min 220, max 420), `sidebarRowHeight` 32,
`navRowHeight` 34, `headerHeight` 44, `contentMaxWidth` 720, `composerMaxWidth` 680,
`composerMaxLines` 12, `iconButton` 28, `avatar` 24.

### 2.4 Motion

`instant` 0.0 · `fast` 0.12 · `normal` 0.2 · `slow` 0.32 · `pulse` 1.2 (repeating).
Curves: `standard` = `.easeOut`, `emphasized` = `.spring(response: 0.35, dampingFraction: 0.82)`.

**Every animation must route through `Theme.motion.animation(_:)`, which returns `nil` when
`NSWorkspace.shared.accessibilityDisplayShouldReduceMotion` is true.** This is how F6.5 is
satisfied globally instead of per-view.

### 2.5 Syntax tokens

Maps `syntect` scope prefixes to colors: `keyword`, `string`, `number`, `comment`,
`function`, `type`, `variable`, `constant`, `operator`, `punctuation`, `attribute`,
`invalid`, plus `plain`. Resolution is longest-prefix-match on the scope string; unmatched
scopes fall back to `plain`.

## 3. Theme plumbing

- `ThemeMode` = `.light | .dark | .system`, persisted in core settings.
- `ThemeController` is an `@Observable` that resolves `ThemeMode` + system appearance into a
  concrete `Theme`, and republishes on `NSApp.effectiveAppearance` changes.
- Injected once at the root via `.environment(\.theme, …)`; changing it crossfades over
  `motion.normal` without disturbing scroll position or first responder (F5.4).
- `Theme` is `Codable`, so alternate themes ship as JSON later (F5.3).

## 4. Primitives to provide

These are consumed by every other UI workstream; build them first and keep them dumb.

`FormButton` (`.primary/.secondary/.ghost/.destructive`, 3 sizes) · `IconButton` ·
`Chip` (label, optional leading icon, optional action) · `SegmentedToggle` ·
`FormTextField` / `FormTextEditor` (autogrow) · `Popover` container · `Sheet` container ·
`ListRow` (hover/selected/pressed states) · `SectionHeader` · `Divider` · `Badge` ·
`ProgressRing` (determinate, animatable, threshold-recoloring) · `ProgressBar` ·
`Shimmer` · `PulsingDot` · `TypingCaret` · `Tooltip` · `EmptyState` (icon, title, message,
optional action) · `Toast` + `ToastCenter` · `Wordmark` (serif `form`, optional size).

Each primitive ships a `#Preview` covering both themes.

## 5. Definition of done

- `swift build` clean; previews render in both themes.
- Contrast test passes for both themes.
- A `NoHardcodedColors` test greps `Sources/FormUI` and `Sources/FormMarkdown` for
  `Color(`, `.red`, `#colorLiteral`, `Font.system(size:` and fails on any hit outside
  `FormDesign`.
- Reduce-motion returns `nil` animations across the board.
