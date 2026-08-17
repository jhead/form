# Spec 04 — Catalog, settings, context accounting (`form-core::catalog`, `::settings`, `::context`)

> **Workstream W4.** Owns `core/crates/form-core/src/catalog/`, `src/settings/`,
> `src/context/`. Satisfies F8, F9, F10.

## 1. Provider catalog

Static data compiled in (`include_str!` a JSON file under
`core/crates/form-core/data/catalog.json`), loaded once behind a `OnceLock`. The shape
mirrors `pi`'s provider/model descriptors closely enough that swapping in `pi-catalog`
later is a source change in one file.

```rust
pub struct Provider {
    pub id: String,                  // "anthropic"
    pub name: String,                // "Anthropic"
    pub base_url: String,
    pub auth: Vec<AuthMethod>,       // apiKey | oauth | none
    pub env_vars: Vec<String>,       // ANTHROPIC_API_KEY
    pub models: Vec<Model>,
}

pub struct Model {
    pub id: String,                  // "claude-opus-5"
    pub name: String,                // "Opus 5"
    pub family: String,
    pub context_window: u64,
    pub max_output: u64,
    pub pricing: Pricing,            // per 1M tokens: input, output, cacheRead, cacheWrite
    pub capabilities: Capabilities,  // vision, tools, reasoning, caching, streaming
    pub thinking_levels: Vec<ThinkingLevel>,
    pub released: Option<String>,
    pub deprecated: bool,
}
```

Cover: **Anthropic** (Opus/Sonnet/Haiku tiers), **OpenAI**, **Google**, **OpenRouter**
(a representative subset plus a note that it proxies others), **xAI**, **Groq**,
**Mistral**, **DeepSeek**, and **Local (Ollama)** with a user-supplied base URL. Pricing
and context windows should be plausible and internally consistent; they feed cost figures
in the dashboard. Mark the data file with a `generatedAt` and a comment that `pi-catalog`
will replace it.

`ThinkingLevel` is `pi`'s ladder exactly: `off, minimal, low, medium, high, xhigh, max`.
Per-model `thinking_levels` filters what the picker offers (F8.2). A model with no
reasoning capability lists only `off`.

Resolution API:

```rust
pub fn resolve(model_ref: &ModelRef) -> Option<&'static Model>;
pub fn parse_ref(s: &str) -> Option<ModelRef>;      // "anthropic/claude-opus-5"
pub fn default_ref() -> ModelRef;
pub fn search(q: &str) -> Vec<ModelHit>;            // fuzzy, for the picker popover
```

## 2. Settings

One document, versioned, persisted as JSON at `{dataDir}/settings.json` (atomic write:
temp file + rename). Not in SQLite — it must be hand-editable and diffable.

```rust
pub struct Settings {
    pub version: u32,
    pub general: GeneralSettings,        // startup view, confirm-on-delete, telemetry opt-in
    pub appearance: AppearanceSettings,  // themeMode, textSizeMultiplier, sidebarWidth,
                                         // sidebarCollapsed, density, showTurnFooters
    pub defaults: DefaultsSettings,      // modelRef, thinkingLevel, systemPrompt,
                                         // toolExecution (sequential|parallel), queueMode
    pub providers: BTreeMap<String, ProviderSettings>,  // enabled, baseUrlOverride,
                                                        // hasKey (bool only — never the key)
    pub editor: EditorSettings,          // font, fontSize, tabWidth, wrapCode, showLineNumbers
    pub advanced: AdvancedSettings,      // logLevel, harnessSpeed, dataDir (read-only display)
    pub shortcuts: BTreeMap<String, String>,  // action id -> key equivalent, overrides only
}
```

**API keys are never in this document and never cross the FFI boundary.** Swift owns
Keychain storage; the core only records `hasKey: bool` per provider so the UI can render
state (F8.5). `updateSettings` replaces the whole document; the core validates, clamps
out-of-range values, fills missing fields from defaults, persists, and emits
`settings_changed` with the normalized document — Swift renders what comes back, not what
it sent.

Provide `export()` / `import(json)` with validation and a defaulted fallback so a corrupt
file never prevents launch (F9.3). On version bump, migrate forward field-by-field.

## 3. Context accounting (F10)

```rust
pub fn context_usage(session: &Session, model: &Model) -> ContextUsage;

pub struct ContextUsage {
    pub used: u64,
    pub total: u64,                  // model.context_window
    pub segments: Vec<ContextSegment>,
    pub cost: Cost,                  // cumulative for the session
    pub message_count: u64,
}
pub struct ContextSegment { pub kind: SegmentKind, pub tokens: u64 }
pub enum SegmentKind { System, Tools, Transcript, Attachments, OutputReserve }
```

Computed from the real transcript, never from a guess in the view (F10.4). `OutputReserve`
is `model.max_output`, shown as a distinct segment so the ring reflects genuinely available
room. Token estimation lives here as `estimate_tokens(&str) -> u64` (chars/4 with a
correction for whitespace and CJK); it is the same function the harness uses, so the numbers
agree. Attachments count at a fixed per-image cost derived from dimensions.

Recompute and emit `context_usage` on every `message_end` and on model change — thresholds
at 75% and 90% are a rendering concern (F10.2), not a core one, but expose
`fraction()` so both sides agree.

## 4. Done when

- Catalog loads, every model resolves by ref, fuzzy search ranks exact-prefix first.
- Settings round-trip through save/load, a corrupt file falls back to defaults with an
  `error` event, unknown fields survive a round trip, and clamping is tested at bounds.
- `context_usage` sums segments to `used`, never exceeds `total` without saturating, and is
  covered by a test over a fixture transcript.
- `cargo clippy -p form-core -- -D warnings` clean.
