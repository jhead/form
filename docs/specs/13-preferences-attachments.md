# Spec 13 — Preferences and attachments (`FormUI/Preferences`, `FormUI/Attachments`)

> **Workstream W13.** Owns `app/Sources/FormUI/Preferences/` and
> `app/Sources/FormUI/Attachments/`. Satisfies F3, F8.3–F8.5, F9, and the picker half of F4.

## Part A — Preferences (F9)

A modal `Sheet` (`⌘,`), 720 × 520, with a leading tab rail (icon + label) and a scrolling
detail pane. Tabs: **General, Providers, Models, Appearance, Editor, Shortcuts, Advanced**.

Every control is bound to `SettingsStore`; edits dispatch `updateSettings` **debounced at
300 ms**, and the view re-renders from the normalized document the core echoes back
(spec 04 §2) — never from local state alone.

| Tab | Contents |
|---|---|
| General | Startup view (Home / last session), confirm before delete, auto-title sessions, queue mode, tool execution mode |
| Providers | One row per provider: enabled toggle, base-URL override, API-key field (secure, shows `••••` when set, Set/Clear buttons), and a `hasKey` state chip. Keys go to the **Keychain via `SettingsStore`; never to the core** (F8.5) |
| Models | Default model + reasoning effort pickers; a searchable table of all models with context window, max output, pricing and capability badges; per-model "set as default" |
| Appearance | Theme mode (Light/Dark/System), text size (0.85–1.4 with a live preview line), density, sidebar width, show turn footers |
| Editor | Code font and size, tab width, wrap code, show line numbers — with a live code sample rendered through `FormMarkdown` |
| Shortcuts | The full table from W14, grouped by category, each row recordable; conflicts flagged inline; Reset to defaults |
| Advanced | Data directory (reveal in Finder), log level, harness speed multiplier, export/import settings JSON, reset all with a typed confirm |

- A secure field must never log or copy its value; the `hasKey` chip is the only readback.
- Import validates and reports errors inline rather than throwing away the file (F9.3).
- Changes apply live with no restart (F9.2) — verify theme, text size and default model.

## Part B — Attachments (F3)

### Intake
Three paths, all landing in one `AttachmentIntake` service:
1. `+` button → `NSOpenPanel` (multiple, files only).
2. Drag-and-drop onto the composer or transcript, with a full-composer drop highlight.
3. Paste (`⌘V`) of file URLs or image data from the pasteboard.

Each item is dispatched as `addAttachment`; the core hashes, dedupes and stores it (spec 01
§4). Rejections (> 10 MB, disallowed mime) surface **inline in the tray** with the reason,
not as a toast (F3.6).

### Thumbnails (F3.2, F3.3)
- Images: decode with `NSImage`/`CGImageSource`, downsample to 128 pt @2× using
  `kCGImageSourceThumbnailMaxPixelSize` (never full-decode a large image), write the PNG to
  `{dataDir}/thumbnails/{sha256}.png`, and record the path via the core.
- PDFs: first page via `PDFKit`. Everything else: a type glyph from `NSWorkspace.icon(for:)`.
- Generation is off the main actor, cached in memory by sha, and reused across sessions —
  the same file attached twice generates one thumbnail.

### Tray and chips
- Pre-send: a horizontal tray above the composer input. Chips are 56 pt tall with a 40 pt
  thumbnail, filename (truncating middle), size, and a remove `×` on hover (F3.5).
- Sent: chips render inside the user message bubble above the text, click-to-open.
- Overlay (F3.4): click opens a full-size viewer — dimmed backdrop, image fit to window,
  `Esc` or click-outside to dismiss, `←`/`→` between attachments in the same message, and a
  Reveal in Finder action.

### Folder picker (F4.1, F4.4)
The composer's folder chip opens a menu: recent roots (from `listRecentRoots`), a
`Choose folder…` item opening `NSOpenPanel` in directory mode, and a `Clear` item. Selecting
dispatches `setWorkspaceRoot`. The chip shows the basename with the full path as a tooltip
(F4.2); an unset root shows `Unconfined` in tertiary (F4.5).

## Done when

- Every settings control round-trips: change, relaunch, still set.
- An API key saves to the Keychain, shows as set, clears, and never appears in any log or in
  `settings.json`.
- Acceptance criterion 9: attaching an image shows a thumbnail, sends, and renders inline.
- Attaching the same file twice produces one stored blob and one thumbnail.
- A 12 MB file is rejected inline with a readable reason.
- The overlay opens, navigates, and dismisses by keyboard alone.
