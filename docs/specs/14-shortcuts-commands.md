# Spec 14 — Shortcuts, command palette, find (`FormUI/Commands`)

> **Workstream W14.** Owns `app/Sources/FormUI/Commands/`. Satisfies F12 and F13's UI half.
> This workstream owns the **single shortcut table** that both the menu bar and the key
> handlers read (F12.3). No other file may declare a `keyboardShortcut`.

## 1. The table

```swift
public struct AppCommand: Identifiable, Sendable {
    public let id: String              // "session.new"
    public let title: String           // "New Chat"
    public let category: Category      // file | edit | view | session | navigate | help
    public let defaultKey: KeyBinding? // .init("n", [.command])
    public let isEnabled: @MainActor (AppState) -> Bool
    public let perform: @MainActor (CommandContext) async -> Void
}

public enum AppCommands { public static let all: [AppCommand] }
```

- Menus are generated from `AppCommands.all` grouped by category (F12.1).
- The global key handler resolves an `NSEvent` against the same table, honoring user
  overrides from `settings.shortcuts` (spec 13, Shortcuts tab).
- The command palette lists the same table.
- A test asserts: unique ids, no duplicate effective bindings after overrides, every command
  reachable from a menu, and every entry in the PRD's F12 table present.

## 2. Bindings (F12)

Exactly as listed in PRD §5/F12. Notes on the tricky ones:

- `⌘[` / `⌘]` traverse **session history** (the route stack), like a browser's back/forward;
  `⌘⌥←` / `⌘⌥→` are aliases.
- `⌘1`–`⌘9` jump to the Nth session in the sidebar's flattened visible order — which is why
  rows show rank numbers (F2.1).
- `Esc` is contextual: dismiss the topmost overlay if one is open, else stop streaming, else
  clear composer focus. Implement as an ordered responder chain, not nested `if`s in a view.
- `⌘W` archives the session rather than closing the window (single-window app); `⌘⇧W` closes
  the window.
- `⌘+`/`⌘-`/`⌘0` adjust `appearance.textSizeMultiplier` through `SettingsStore`.

## 3. Command palette (`⌘K`, F13.1)

- A centered overlay panel, 640 pt wide, appearing at 20% from the top with a
  `motion.emphasized` scale-and-fade.
- One query field. Results are three sections, each capped and headed: **Sessions**
  (from `searchSessions`), **Commands** (fuzzy over `AppCommands.all`), **Groups**.
- Session hits show the title, group name, relative time, and a snippet with match ranges
  highlighted from the core's `{start,len}` ranges (spec 01 §4) — never re-search in Swift.
- `↑`/`↓` move, `⏎` opens, `⌘⏎` opens in a new session where meaningful, `Esc` dismisses.
  The first result is preselected.
- Queries are debounced 120 ms and cancelled on change; results never arrive out of order.
- Empty query shows recent sessions and the most-used commands.

## 4. Find in session (`⌘F`, F13.2)

- A find bar docked under the session header: field, `n of m` count, previous/next buttons,
  case-sensitive and whole-word toggles, and a close button.
- Backed by `searchInSession`. All matches highlight in `accentMuted`; the current match uses
  `accent` and scrolls into view with a brief flash.
- `⌘G` / `⌘⇧G` and `⏎` / `⇧⏎` step matches, wrapping with a subtle bounce at the ends.
- `Esc` closes and clears highlights, restoring composer focus.
- Opening find with a text selection seeds the query from it.

## 5. Cheat sheet (`⌘/`, F12.2)

A modal overlay listing every command grouped by category with its rendered key equivalent
(proper `⌘⇧⌥⌃` glyphs), two columns, searchable, `Esc` to dismiss.

## 6. Done when

- Acceptance criterion 6: every F12 shortcut works and appears in the menu bar.
- The table test passes, including with a user override applied.
- Palette search over the seeded corpus returns ranked hits with correct highlight ranges in
  under 50 ms.
- Find highlights all matches, steps and wraps correctly, and survives a streaming update
  without losing the current match.
- Everything in this spec is operable with no mouse.
