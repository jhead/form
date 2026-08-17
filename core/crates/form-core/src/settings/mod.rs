//! The settings document.
//!
//! **Owner: W4** (`docs/specs/04-catalog-settings.md`).
//!
//! API keys are **never** in this document and never cross the FFI boundary — Swift owns
//! Keychain storage and the core records only `hasKey`. W4 adds the remaining sections
//! (editor, advanced, shortcuts), validation, clamping, atomic persistence and migration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::default_model_ref;
use crate::error::Result;
use crate::protocol::ModelRef;

pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSettings {
    pub theme_mode: ThemeMode,
    pub text_size_multiplier: f64,
    pub sidebar_width: f64,
    pub sidebar_collapsed: bool,
    pub show_turn_footers: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::System,
            text_size_multiplier: 1.0,
            sidebar_width: 300.0,
            sidebar_collapsed: false,
            show_turn_footers: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneralSettings {
    pub startup_view: String,
    pub confirm_on_delete: bool,
    pub auto_title_sessions: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            startup_view: "home".to_string(),
            confirm_on_delete: true,
            auto_title_sessions: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultsSettings {
    pub model_ref: ModelRef,
    pub system_prompt: String,
}

impl Default for DefaultsSettings {
    fn default() -> Self {
        Self {
            model_ref: default_model_ref(),
            system_prompt: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettings {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url_override: Option<String>,
    /// Presence only. The key itself lives in the macOS Keychain, owned by Swift.
    pub has_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub version: u32,
    pub general: GeneralSettings,
    pub appearance: AppearanceSettings,
    pub defaults: DefaultsSettings,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSettings>,
    // TODO(W4): editor, advanced, shortcuts sections.
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            general: GeneralSettings::default(),
            appearance: AppearanceSettings::default(),
            defaults: DefaultsSettings::default(),
            providers: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Clamp out-of-range values rather than rejecting the document — the app renders what
    /// the core echoes back, so normalization has to happen here.
    pub fn normalize(&mut self) {
        self.version = SETTINGS_VERSION;
        self.appearance.text_size_multiplier =
            self.appearance.text_size_multiplier.clamp(0.85, 1.4);
        self.appearance.sidebar_width = self.appearance.sidebar_width.clamp(220.0, 420.0);
    }
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("settings.json"),
        }
    }

    /// A corrupt file must never prevent launch (F9.3).
    pub fn load(&self) -> Settings {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .map(|mut s| {
                s.normalize();
                s
            })
            .unwrap_or_default()
    }

    pub fn save(&self, settings: &Settings) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(settings)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
