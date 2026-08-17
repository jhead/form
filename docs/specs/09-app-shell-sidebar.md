# Spec 09 — App shell and sidebar (`FormUI/Shell`, `FormUI/Sidebar`)

> **Workstream W9.** Owns `app/Sources/FormUI/Shell/`, `app/Sources/FormUI/Sidebar/`, and
> `app/Sources/form/` (the executable's `App` entry point). Satisfies F2 and the shell half
> of F5/F12. Read [spec 08 §1](./08-design-system.md) — it describes the reference layout in
> words, and that description is the acceptance bar.

## 1. Window

- `WindowGroup` with `.windowStyle(.hiddenTitleBar)`, single window, min size 900 × 600,
  default 1280 × 860, frame autosaved.
- `NavigationSplitView` with a resizable sidebar (`metrics.sidebarWidth`, 220–420) whose
  width and collapsed state persist through `SettingsStore` (F2.7).
- Traffic lights overlay the sidebar; the first sidebar row reserves 78 pt of leading space
  for them.
- Content root switches on `AppRoute`: `.home` or `.session(id)`.

## 2. Sidebar structure

Top to bottom, matching spec 08 §1:

1. **Control row** — sidebar-toggle and search icon buttons, trailing-aligned.
2. **`SegmentedToggle`** — `Home` / `Code`. `Home` routes to the dashboard; `Code` restores
   the last selected session (or an empty state if none).
3. **`New` row** — `+ New`, `⌘N`.
4. **Group sections** — one per `SessionGroup` plus a trailing `Ungrouped` section. Header:
   name, hover disclosure chevron, trailing `⋯` menu (rename, new session in group, delete
   group). Collapsed state persists per group.
5. **Session rows** — see §3.
6. **Footer** — monogram avatar, display name, `·`, active provider label, trailing chevron
   opening a menu (Preferences `⌘,`, Appearance submenu, About, Quit).

Empty group → a 32 pt `Drag or move sessions here` drop target (F2.2).

## 3. Session row

- Leading 16 pt slot: rank number (tabular, tertiary) for the first 9 rows overall; swaps to
  a 6 pt status dot on hover or when the session is streaming or errored (F2.4).
- Title, 13 pt, tail-truncated. Streaming rows animate a `PulsingDot` (F6.1).
- Selected: `surfaceSelected` fill, primary text. Hover: `surfaceHover`.
- Inline rename on double-click or `⏎` (F2.5): a borderless field in place; `⏎` commits,
  `Esc` cancels, focus loss commits.
- Context menu: Rename, Duplicate, Move to ▸ (groups + New group…), Pin, Archive, Delete
  (with confirm when `general.confirmOnDelete`).
- Drag and drop (F2.3): `.draggable` with a session-id transferable; drop targets are group
  sections and inter-row insertion points, with a 2 pt insertion indicator. Reorder is
  applied optimistically and dispatched as `moveSession`; the store reconciles.

## 4. Content shell

- **Home** — hosts `HomeView` from W12.
- **Session** — a 44 pt header (title, workspace chip, trailing icon buttons and `⋮`
  overflow) above `ChatView` from W10. The header's title is editable in place.
- **Empty** — `EmptyState` with the serif `Wordmark`, a one-line hint, and a `New chat`
  button.
- A `ToastCenter` overlay at the top trailing edge renders `error` events (spec 00 §5.2).

## 5. Routing and state

```swift
@Observable @MainActor final class AppState {
    var route: AppRoute
    var sidebarCollapsed: Bool
    var searchPresented: Bool          // ⌘K, owned by W14
    var findPresented: Bool            // ⌘F, owned by W14
}
```

`AppState` is created at the root and injected via `.environment`. W14 mutates the two
presentation flags; W9 owns everything else. Route changes push onto a bounded history stack
so `⌘[` / `⌘]` traverse it (F12).

Restore on launch (acceptance criterion 5): last route, sidebar width and collapsed state,
group collapse states, and per-session scroll offset (persisted by W10, restored here on
route change).

## 6. Done when

- Layout matches spec 08 §1 in both themes at 1280 × 860 and at the minimum window size.
- Drag a session between groups; relaunch; it stayed (acceptance criterion 8).
- Sidebar collapse, width, group collapse and selection all survive relaunch.
- Every action in this spec has a menu-bar item with its key equivalent, sourced from W14's
  shortcut table — no duplicated key definitions (F12.3).
- VoiceOver: rows expose title, status, and rank; the segmented control and buttons are
  labeled.
