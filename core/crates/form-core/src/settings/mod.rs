//! The settings document.
//!
//! **Owner: W4** (`docs/specs/04-catalog-settings.md` §2).
//!
//! One versioned JSON document at `{dataDir}/settings.json`, written atomically and meant to
//! be hand-editable. Three rules shape everything here:
//!
//! 1. **API keys are never in this document and never cross the FFI boundary.** Swift owns
//!    Keychain storage; the core records only `hasKey` per provider (F8.5).
//! 2. **The core normalizes, the app renders what comes back.** `updateSettings` replaces the
//!    whole document, so every out-of-range value is clamped rather than rejected — there is
//!    no error path that would leave the UI showing something the core did not accept.
//! 3. **A corrupt file never prevents launch** (F9.3). It is backed up, reported, and
//!    replaced by defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::app::default_model_ref;
use crate::catalog;
use crate::error::Result;
use crate::protocol::ModelRef;

#[cfg(test)]
mod tests;

/// Bump when the document's shape changes, and add a step to [`migrate_document`].
pub const SETTINGS_VERSION: u32 = 1;

/// Unrecognized keys are carried through untouched so a document written by a newer build
/// survives a round trip through an older one.
type Extra = Map<String, Value>;

/// A small closed vocabulary that must survive a garbage value in a hand-edited file:
/// an unknown string deserializes to the default rather than failing the whole document.
macro_rules! wire_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? } default $default:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                let s = s.trim();
                $(if s.eq_ignore_ascii_case($wire) { return Some(Self::$variant); })+
                None
            }
        }

        impl Default for $name {
            fn default() -> Self { Self::$default }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                let value = Value::deserialize(d)?;
                Ok(value.as_str().and_then(Self::parse).unwrap_or_default())
            }
        }
    };
}

wire_enum!(ThemeMode { Light => "light", Dark => "dark", System => "system" } default System);
wire_enum!(Density { Comfortable => "comfortable", Compact => "compact" } default Comfortable);
wire_enum!(StartupView { Home => "home", LastSession => "lastSession" } default Home);
wire_enum!(
    /// Whether a turn's tool calls run one at a time or together (F8/F1).
    ToolExecution { Sequential => "sequential", Parallel => "parallel" } default Parallel
);
wire_enum!(
    /// What sending during a live run does (F1.7).
    QueueMode { Queue => "queue", Interrupt => "interrupt" } default Queue
);
wire_enum!(
    LogLevel { Error => "error", Warn => "warn", Info => "info", Debug => "debug", Trace => "trace" }
    default Info
);

// ---------------------------------------------------------------- sections

// `#[serde(default)]` on a field uses the *type's* default, which would turn an absent
// `sidebarWidth` into 0.0 rather than 300.0. Every field whose default is not the type's
// zero value names one of these instead, and the `Default` impls reuse them so there is one
// source of truth per value.
fn yes() -> bool {
    true
}
fn default_text_size() -> f64 {
    1.0
}
fn default_sidebar_width() -> f64 {
    300.0
}
fn default_font() -> String {
    "SF Mono".to_string()
}
fn default_font_size() -> f64 {
    12.0
}
fn default_tab_width() -> u32 {
    4
}
fn default_harness_speed() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneralSettings {
    #[serde(default)]
    pub startup_view: StartupView,
    #[serde(default = "yes")]
    pub confirm_on_delete: bool,
    #[serde(default = "yes")]
    pub auto_title_sessions: bool,
    /// Opt-in, off by default, and nothing reads it yet.
    #[serde(default)]
    pub telemetry: bool,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            startup_view: StartupView::Home,
            confirm_on_delete: true,
            auto_title_sessions: true,
            telemetry: false,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSettings {
    #[serde(default)]
    pub theme_mode: ThemeMode,
    #[serde(default = "default_text_size")]
    pub text_size_multiplier: f64,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f64,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default)]
    pub density: Density,
    #[serde(default = "yes")]
    pub show_turn_footers: bool,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::System,
            text_size_multiplier: default_text_size(),
            sidebar_width: default_sidebar_width(),
            sidebar_collapsed: false,
            density: Density::Comfortable,
            show_turn_footers: true,
            extra: Extra::new(),
        }
    }
}

pub const TEXT_SIZE_RANGE: (f64, f64) = (0.85, 1.4);
pub const SIDEBAR_WIDTH_RANGE: (f64, f64) = (220.0, 420.0);
pub const FONT_SIZE_RANGE: (f64, f64) = (9.0, 24.0);
pub const TAB_WIDTH_RANGE: (u32, u32) = (1, 8);
pub const HARNESS_SPEED_RANGE: (f64, f64) = (0.05, 200.0);
/// A system prompt longer than this is a paste accident, not a preference.
pub const SYSTEM_PROMPT_MAX_CHARS: usize = 32_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultsSettings {
    /// Carries the default thinking level as part of the ref, exactly as a session does.
    #[serde(default = "default_model_ref")]
    pub model_ref: ModelRef,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tool_execution: ToolExecution,
    #[serde(default)]
    pub queue_mode: QueueMode,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for DefaultsSettings {
    fn default() -> Self {
        Self {
            model_ref: default_model_ref(),
            system_prompt: String::new(),
            tool_execution: ToolExecution::Parallel,
            queue_mode: QueueMode::Queue,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_override: Option<String>,
    /// Presence only. The key itself lives in the macOS Keychain, owned by Swift, and must
    /// never appear in this document or anywhere on the FFI boundary (F8.5).
    #[serde(default)]
    pub has_key: bool,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url_override: None,
            has_key: false,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSettings {
    /// Empty means "whatever `FormDesign` calls the default monospace face".
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default = "default_font_size")]
    pub font_size: f64,
    #[serde(default = "default_tab_width")]
    pub tab_width: u32,
    #[serde(default)]
    pub wrap_code: bool,
    #[serde(default = "yes")]
    pub show_line_numbers: bool,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font: default_font(),
            font_size: default_font_size(),
            tab_width: default_tab_width(),
            wrap_code: false,
            show_line_numbers: true,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedSettings {
    #[serde(default)]
    pub log_level: LogLevel,
    /// Multiplier on stub-harness timings; mirrors `CoreConfig::harness_speed`.
    #[serde(default = "default_harness_speed")]
    pub harness_speed: f64,
    /// Read-only display value. The store stamps it on load; the app shows it and echoes it
    /// back unchanged.
    #[serde(default)]
    pub data_dir: String,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            harness_speed: default_harness_speed(),
            data_dir: String::new(),
            extra: Extra::new(),
        }
    }
}

// ---------------------------------------------------------------- document

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub defaults: DefaultsSettings,
    /// Keyed by catalog provider id. `normalize` fills in an entry for every known provider
    /// so the Providers tab can render without consulting the catalog for presence.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSettings>,
    #[serde(default)]
    pub editor: EditorSettings,
    #[serde(default)]
    pub advanced: AdvancedSettings,
    /// Overrides only: action id -> key equivalent. An absent entry means "use the default
    /// from `AppCommands`" (spec 14 §1).
    #[serde(default)]
    pub shortcuts: BTreeMap<String, String>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

impl Default for Settings {
    fn default() -> Self {
        let mut settings = Self {
            version: SETTINGS_VERSION,
            general: GeneralSettings::default(),
            appearance: AppearanceSettings::default(),
            defaults: DefaultsSettings::default(),
            providers: BTreeMap::new(),
            editor: EditorSettings::default(),
            advanced: AdvancedSettings::default(),
            shortcuts: BTreeMap::new(),
            extra: Extra::new(),
        };
        settings.normalize();
        settings
    }
}

/// Something worth telling the user about, shaped like the `error` event's payload so the
/// wiring in `core.rs` is a straight move (spec 00 §5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsIssue {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl SettingsIssue {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            detail: None,
        }
    }

    fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl Settings {
    /// Clamp out-of-range values rather than rejecting the document — the app renders what
    /// the core echoes back, so normalization has to happen here.
    pub fn normalize(&mut self) {
        self.normalize_reporting();
    }

    /// The same pass, reporting what it had to change. Import surfaces these inline (F9.3);
    /// `updateSettings` ignores them because the echoed document already shows the result.
    pub fn normalize_reporting(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        self.version = SETTINGS_VERSION;

        // --- appearance ---
        clamp_f64(
            &mut self.appearance.text_size_multiplier,
            TEXT_SIZE_RANGE,
            default_text_size(),
            "appearance.textSizeMultiplier",
            &mut notes,
        );
        clamp_f64(
            &mut self.appearance.sidebar_width,
            SIDEBAR_WIDTH_RANGE,
            default_sidebar_width(),
            "appearance.sidebarWidth",
            &mut notes,
        );

        // --- editor ---
        clamp_f64(
            &mut self.editor.font_size,
            FONT_SIZE_RANGE,
            default_font_size(),
            "editor.fontSize",
            &mut notes,
        );
        let tab_width = self
            .editor
            .tab_width
            .clamp(TAB_WIDTH_RANGE.0, TAB_WIDTH_RANGE.1);
        if tab_width != self.editor.tab_width {
            notes.push(format!(
                "editor.tabWidth {} clamped to {tab_width}",
                self.editor.tab_width
            ));
            self.editor.tab_width = tab_width;
        }
        let font = self.editor.font.trim().to_string();
        self.editor.font = if font.is_empty() {
            default_font()
        } else {
            font
        };

        // --- advanced ---
        clamp_f64(
            &mut self.advanced.harness_speed,
            HARNESS_SPEED_RANGE,
            default_harness_speed(),
            "advanced.harnessSpeed",
            &mut notes,
        );
        self.advanced.data_dir = self.advanced.data_dir.trim().to_string();

        // --- defaults ---
        if self.defaults.system_prompt.chars().count() > SYSTEM_PROMPT_MAX_CHARS {
            self.defaults.system_prompt = self
                .defaults
                .system_prompt
                .chars()
                .take(SYSTEM_PROMPT_MAX_CHARS)
                .collect();
            notes.push(format!(
                "defaults.systemPrompt truncated to {SYSTEM_PROMPT_MAX_CHARS} characters"
            ));
        }
        self.normalize_model_ref(&mut notes);

        // --- providers ---
        for (id, provider) in self.providers.iter_mut() {
            if let Some(url) = &provider.base_url_override {
                let trimmed = url.trim();
                provider.base_url_override = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
            // Defence in depth: a key must never reach this document, whatever the app sends.
            for key in ["apiKey", "api_key", "key", "token", "secret"] {
                if provider.extra.remove(key).is_some() {
                    notes.push(format!(
                        "providers.{id}.{key} dropped — credentials never enter settings.json"
                    ));
                }
            }
        }
        for provider in catalog::providers() {
            self.providers.entry(provider.id.clone()).or_default();
        }

        // --- shortcuts ---
        let cleaned: BTreeMap<String, String> = std::mem::take(&mut self.shortcuts)
            .into_iter()
            .filter_map(|(action, key)| {
                let (action, key) = (action.trim().to_string(), key.trim().to_string());
                (!action.is_empty() && !key.is_empty()).then_some((action, key))
            })
            .collect();
        self.shortcuts = cleaned;

        notes
    }

    /// Keep the default model pointing at something the catalog can price and size. An
    /// unknown *model* under a known provider is kept — Ollama and OpenRouter lists are open
    /// — but an unknown provider falls back, and the effort snaps onto the model's ladder.
    fn normalize_model_ref(&mut self, notes: &mut Vec<String>) {
        let model_ref = &mut self.defaults.model_ref;
        if !catalog::is_known_provider(&model_ref.provider_id) {
            notes.push(format!(
                "defaults.modelRef provider `{}` is not in the catalog; using the default model",
                model_ref.provider_id
            ));
            *model_ref = default_model_ref();
            return;
        }
        if let Some(model) = catalog::resolve_ref(model_ref) {
            let clamped = model.clamp_thinking_level(model_ref.thinking_level);
            if clamped != model_ref.thinking_level {
                notes.push(format!(
                    "defaults.modelRef thinking level {} is not offered by {}; using {}",
                    model_ref.thinking_level.as_str(),
                    model.name,
                    clamped.as_str()
                ));
                model_ref.thinking_level = clamped;
            }
        }
    }

    /// Pretty JSON for the Advanced tab's export button (F9.3).
    pub fn export(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Import a document, migrating and normalizing it. Errors are returned rather than
    /// swallowed so the sheet can show them inline instead of discarding the file.
    pub fn import(json: &str) -> std::result::Result<(Settings, Vec<String>), SettingsIssue> {
        let value: Value = serde_json::from_str(json)
            .map_err(|e| SettingsIssue::new("settings_invalid_json", e.to_string()))?;
        if !value.is_object() {
            return Err(SettingsIssue::new(
                "settings_invalid_json",
                "settings must be a JSON object",
            ));
        }
        let mut value = value;
        migrate_document(&mut value);
        let mut settings: Settings = serde_json::from_value(value)
            .map_err(|e| SettingsIssue::new("settings_invalid_shape", e.to_string()))?;
        // The data directory is a property of this machine, not of the document.
        settings.advanced.data_dir.clear();
        let notes = settings.normalize_reporting();
        Ok((settings, notes))
    }

    /// Import that cannot fail: a bad document yields defaults plus the reason.
    pub fn import_or_default(json: &str) -> (Settings, Option<SettingsIssue>) {
        match Self::import(json) {
            Ok((settings, _)) => (settings, None),
            Err(issue) => (Settings::default(), Some(issue)),
        }
    }
}

fn clamp_f64(
    value: &mut f64,
    range: (f64, f64),
    fallback: f64,
    field: &str,
    notes: &mut Vec<String>,
) {
    let original = *value;
    *value = if original.is_finite() {
        original.clamp(range.0, range.1)
    } else {
        fallback
    };
    if *value != original {
        notes.push(format!("{field} {original} clamped to {}", *value));
    }
}

// ---------------------------------------------------------------- migration

/// Raise a document to [`SETTINGS_VERSION`], one step at a time, in place. Steps only touch
/// the fields they know about, so anything unrecognized rides along untouched.
pub fn migrate_document(doc: &mut Value) {
    let mut version = doc
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;

    while version < SETTINGS_VERSION {
        match version {
            0 => migrate_0_to_1(doc),
            // Add the next step here; the loop does the rest.
            _ => break,
        }
        version += 1;
    }

    if let Some(object) = doc.as_object_mut() {
        object.insert("version".to_string(), Value::from(SETTINGS_VERSION));
    }
}

/// v0 is the unversioned document early dev builds wrote: a flat `defaults.model` string
/// with the effort beside it, and `appearance.theme` instead of `themeMode`.
fn migrate_0_to_1(doc: &mut Value) {
    if let Some(defaults) = doc.get_mut("defaults").and_then(Value::as_object_mut) {
        if !defaults.contains_key("modelRef") {
            let model = defaults.get("model").and_then(Value::as_str).unwrap_or("");
            let level = defaults
                .get("thinkingLevel")
                .and_then(Value::as_str)
                .unwrap_or("");
            let model_ref = catalog::parse_ref(&if level.is_empty() {
                model.to_string()
            } else {
                format!("{model}:{level}")
            })
            .unwrap_or_else(default_model_ref);
            if let Ok(value) = serde_json::to_value(model_ref) {
                defaults.insert("modelRef".to_string(), value);
            }
        }
        defaults.remove("model");
        defaults.remove("thinkingLevel");
    }
    if let Some(appearance) = doc.get_mut("appearance").and_then(Value::as_object_mut) {
        if let Some(theme) = appearance.remove("theme") {
            appearance.entry("themeMode").or_insert(theme);
        }
    }
}

// ---------------------------------------------------------------- store

/// Atomic, hand-editable persistence. Not SQLite: this file is meant to be diffable and
/// fixable with a text editor (spec 04 §2).
pub struct SettingsStore {
    path: PathBuf,
    data_dir: PathBuf,
}

/// What [`SettingsStore::load_reporting`] found. `issue` is non-`None` exactly when the file
/// existed but could not be used.
pub struct LoadOutcome {
    pub settings: Settings,
    pub issue: Option<SettingsIssue>,
}

impl SettingsStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("settings.json"),
            data_dir: data_dir.to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A corrupt file must never prevent launch (F9.3).
    pub fn load(&self) -> Settings {
        self.load_reporting().settings
    }

    /// Load, reporting why defaults were used. The caller emits an `error` event with the
    /// issue so the corruption is visible instead of silent.
    pub fn load_reporting(&self) -> LoadOutcome {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            // No file yet is the normal first launch, not an error.
            Err(_) => return self.outcome(Settings::default(), None),
        };

        match Settings::import(&raw) {
            Ok((settings, _)) => self.outcome(settings, None),
            Err(issue) => {
                // Keep the user's file: it is hand-editable, so a typo should be recoverable.
                let backup = self.backup_corrupt_file();
                let issue = issue.with_detail(serde_json::json!({
                    "path": self.path.to_string_lossy(),
                    "backup": backup.as_ref().map(|p| p.to_string_lossy()),
                }));
                self.outcome(Settings::default(), Some(issue))
            }
        }
    }

    fn outcome(&self, mut settings: Settings, issue: Option<SettingsIssue>) -> LoadOutcome {
        self.normalize(&mut settings);
        LoadOutcome { settings, issue }
    }

    /// `Settings::normalize` plus the machine-local fields only the store knows.
    pub fn normalize(&self, settings: &mut Settings) {
        settings.normalize();
        settings.advanced.data_dir = self.data_dir.to_string_lossy().into_owned();
    }

    fn backup_corrupt_file(&self) -> Option<PathBuf> {
        let stamp = crate::protocol::now_ms();
        let backup = self.path.with_extension(format!("corrupt-{stamp}.json"));
        std::fs::rename(&self.path, &backup).ok().map(|()| backup)
    }

    /// Temp file plus rename, so a crash mid-write cannot truncate the document.
    pub fn save(&self, settings: &Settings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(settings)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
