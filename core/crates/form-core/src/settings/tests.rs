use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::protocol::ThinkingLevel;

/// A scratch directory that cleans up after itself; the store is filesystem-backed by design.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "form-settings-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn store(&self) -> SettingsStore {
        SettingsStore::new(&self.0)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn defaults_are_complete_and_stable() {
    let settings = Settings::default();
    assert_eq!(settings.version, SETTINGS_VERSION);
    assert_eq!(settings.appearance.theme_mode, ThemeMode::System);
    assert_eq!(settings.editor.tab_width, 4);
    assert_eq!(settings.advanced.log_level, LogLevel::Info);
    assert_eq!(settings.general.startup_view, StartupView::Home);
    assert_eq!(settings.defaults.queue_mode, QueueMode::Queue);
    assert!(settings.shortcuts.is_empty(), "overrides only");

    // Every catalog provider gets a row so the Providers tab has state to render.
    for provider in crate::catalog::providers() {
        let entry = settings
            .providers
            .get(&provider.id)
            .unwrap_or_else(|| panic!("no settings row for {}", provider.id));
        assert!(entry.enabled);
        assert!(!entry.has_key);
    }
}

#[test]
fn every_preferences_control_has_a_field() {
    // Spec 13's tabs, one assertion per control that the sheet binds.
    let json = serde_json::to_value(Settings::default()).unwrap();
    for pointer in [
        "/general/startupView",
        "/general/confirmOnDelete",
        "/general/autoTitleSessions",
        "/general/telemetry",
        "/defaults/modelRef",
        "/defaults/systemPrompt",
        "/defaults/toolExecution",
        "/defaults/queueMode",
        "/appearance/themeMode",
        "/appearance/textSizeMultiplier",
        "/appearance/density",
        "/appearance/sidebarWidth",
        "/appearance/sidebarCollapsed",
        "/appearance/showTurnFooters",
        "/editor/font",
        "/editor/fontSize",
        "/editor/tabWidth",
        "/editor/wrapCode",
        "/editor/showLineNumbers",
        "/advanced/logLevel",
        "/advanced/harnessSpeed",
        "/advanced/dataDir",
        "/providers/anthropic/enabled",
        "/providers/anthropic/hasKey",
        "/shortcuts",
    ] {
        assert!(json.pointer(pointer).is_some(), "missing {pointer}");
    }
}

#[test]
fn clamping_is_applied_at_the_bounds() {
    let mut settings = Settings::default();
    settings.appearance.text_size_multiplier = 4.0;
    settings.appearance.sidebar_width = 10.0;
    settings.editor.font_size = 100.0;
    settings.editor.tab_width = 99;
    settings.advanced.harness_speed = -3.0;
    let notes = settings.normalize_reporting();

    assert_eq!(settings.appearance.text_size_multiplier, TEXT_SIZE_RANGE.1);
    assert_eq!(settings.appearance.sidebar_width, SIDEBAR_WIDTH_RANGE.0);
    assert_eq!(settings.editor.font_size, FONT_SIZE_RANGE.1);
    assert_eq!(settings.editor.tab_width, TAB_WIDTH_RANGE.1);
    assert_eq!(settings.advanced.harness_speed, HARNESS_SPEED_RANGE.0);
    assert_eq!(notes.len(), 5, "each clamp is reported: {notes:?}");

    // In-range values at the exact bounds are left alone and reported as clean.
    let mut settings = Settings::default();
    settings.appearance.text_size_multiplier = TEXT_SIZE_RANGE.0;
    settings.appearance.sidebar_width = SIDEBAR_WIDTH_RANGE.1;
    settings.editor.font_size = FONT_SIZE_RANGE.0;
    settings.editor.tab_width = TAB_WIDTH_RANGE.0;
    assert!(settings.normalize_reporting().is_empty());
    assert_eq!(settings.appearance.text_size_multiplier, TEXT_SIZE_RANGE.0);
    assert_eq!(settings.appearance.sidebar_width, SIDEBAR_WIDTH_RANGE.1);

    // NaN cannot be clamped into a range; it falls back to the default.
    let mut settings = Settings::default();
    settings.appearance.text_size_multiplier = f64::NAN;
    settings.normalize();
    assert_eq!(settings.appearance.text_size_multiplier, 1.0);
}

#[test]
fn normalize_repairs_the_default_model_ref() {
    let mut settings = Settings::default();
    settings.defaults.model_ref = ModelRef {
        provider_id: "made-up".into(),
        model_id: "whatever".into(),
        thinking_level: ThinkingLevel::Max,
    };
    settings.normalize();
    assert_eq!(settings.defaults.model_ref, default_model_ref());

    // Known provider, unknown model: kept, because local model lists are open.
    settings.defaults.model_ref = ModelRef {
        provider_id: "ollama".into(),
        model_id: "mystery:7b".into(),
        thinking_level: ThinkingLevel::High,
    };
    settings.normalize();
    assert_eq!(settings.defaults.model_ref.model_id, "mystery:7b");

    // Known model, effort it does not offer: snapped onto its ladder.
    settings.defaults.model_ref = ModelRef {
        provider_id: "anthropic".into(),
        model_id: "claude-opus-5".into(),
        thinking_level: ThinkingLevel::Xhigh,
    };
    settings.normalize();
    assert_eq!(
        settings.defaults.model_ref.thinking_level,
        ThinkingLevel::High
    );
}

#[test]
fn normalize_cleans_providers_and_shortcuts() {
    let mut settings = Settings::default();
    settings.providers.insert(
        "anthropic".to_string(),
        ProviderSettings {
            enabled: true,
            base_url_override: Some("   ".to_string()),
            has_key: true,
            extra: [("apiKey".to_string(), json!("sk-ant-secret"))]
                .into_iter()
                .collect(),
        },
    );
    settings
        .shortcuts
        .insert("  session.new  ".to_string(), " cmd+n ".to_string());
    settings.shortcuts.insert("session.close".into(), "".into());
    let notes = settings.normalize_reporting();

    let anthropic = &settings.providers["anthropic"];
    assert_eq!(anthropic.base_url_override, None, "blank override cleared");
    assert!(anthropic.has_key, "presence flag survives");
    assert!(
        anthropic.extra.is_empty(),
        "a key smuggled into the document is dropped"
    );
    assert!(notes.iter().any(|n| n.contains("apiKey")));

    assert_eq!(settings.shortcuts.get("session.new").unwrap(), "cmd+n");
    assert!(!settings.shortcuts.contains_key("session.close"));
    assert!(!settings.export().contains("sk-ant-secret"));
}

#[test]
fn unknown_fields_survive_a_round_trip() {
    let raw = json!({
        "version": SETTINGS_VERSION,
        "futureSection": { "nested": [1, 2, 3] },
        "general": { "startupView": "home", "futureFlag": true },
        "appearance": { "themeMode": "dark", "accentHue": 210 },
        "providers": { "anthropic": { "enabled": true, "hasKey": true, "futureField": "x" } }
    })
    .to_string();

    let (settings, _) = Settings::import(&raw).unwrap();
    let round_tripped: Value = serde_json::from_str(&settings.export()).unwrap();

    assert_eq!(round_tripped["futureSection"]["nested"], json!([1, 2, 3]));
    assert_eq!(round_tripped["general"]["futureFlag"], json!(true));
    assert_eq!(round_tripped["appearance"]["accentHue"], json!(210));
    assert_eq!(
        round_tripped["providers"]["anthropic"]["futureField"],
        json!("x")
    );
    assert_eq!(round_tripped["appearance"]["themeMode"], json!("dark"));
}

#[test]
fn unknown_enum_values_fall_back_instead_of_failing_the_document() {
    let raw = json!({
        "version": SETTINGS_VERSION,
        "appearance": { "themeMode": "sepia", "density": 7 },
        "advanced": { "logLevel": "chatty" },
        "general": { "startupView": "lastSession" }
    })
    .to_string();

    let (settings, _) = Settings::import(&raw).unwrap();
    assert_eq!(settings.appearance.theme_mode, ThemeMode::System);
    assert_eq!(settings.appearance.density, Density::Comfortable);
    assert_eq!(settings.advanced.log_level, LogLevel::Info);
    assert_eq!(settings.general.startup_view, StartupView::LastSession);
}

#[test]
fn partial_documents_fill_from_defaults() {
    let (settings, _) = Settings::import(r#"{"appearance":{"themeMode":"dark"}}"#).unwrap();
    assert_eq!(settings.appearance.theme_mode, ThemeMode::Dark);
    assert_eq!(settings.appearance.sidebar_width, 300.0);
    assert_eq!(settings.editor.tab_width, 4);
    assert_eq!(settings.defaults.model_ref, default_model_ref());
    assert_eq!(settings.version, SETTINGS_VERSION);
}

#[test]
fn version_0_documents_migrate_forward() {
    let raw = json!({
        "defaults": { "model": "openai/gpt-5.1", "thinkingLevel": "xhigh", "systemPrompt": "be brief" },
        "appearance": { "theme": "dark", "sidebarWidth": 260.0 }
    })
    .to_string();

    let (settings, _) = Settings::import(&raw).unwrap();
    assert_eq!(settings.version, SETTINGS_VERSION);
    assert_eq!(settings.defaults.model_ref.provider_id, "openai");
    assert_eq!(settings.defaults.model_ref.model_id, "gpt-5.1");
    assert_eq!(
        settings.defaults.model_ref.thinking_level,
        ThinkingLevel::Xhigh,
        "gpt-5.1 offers xhigh, so the level carries over intact"
    );
    assert_eq!(settings.defaults.system_prompt, "be brief");
    assert_eq!(settings.appearance.theme_mode, ThemeMode::Dark);
    assert_eq!(settings.appearance.sidebar_width, 260.0);

    // A v0 document naming a model that no longer exists still lands somewhere usable.
    let raw = json!({ "defaults": { "model": "gone/model-x" } }).to_string();
    let (settings, _) = Settings::import(&raw).unwrap();
    assert_eq!(settings.defaults.model_ref, default_model_ref());
}

#[test]
fn import_rejects_garbage_and_import_or_default_recovers() {
    assert!(Settings::import("not json at all").is_err());
    assert!(Settings::import("[1,2,3]").is_err());

    let (settings, issue) = Settings::import_or_default("{ oops");
    assert_eq!(settings.version, SETTINGS_VERSION);
    assert_eq!(issue.unwrap().code, "settings_invalid_json");

    let (_, issue) = Settings::import_or_default(&Settings::default().export());
    assert!(issue.is_none());
}

#[test]
fn import_drops_the_machine_local_data_dir() {
    let mut settings = Settings::default();
    settings.advanced.data_dir = "/Users/someone-else/Library/Application Support/form".into();
    let (imported, _) = Settings::import(&settings.export()).unwrap();
    assert!(imported.advanced.data_dir.is_empty());
}

#[test]
fn store_round_trips_through_save_and_load() {
    let dir = TempDir::new("round-trip");
    let store = dir.store();

    let mut settings = store.load();
    assert_eq!(
        settings.advanced.data_dir,
        dir.0.to_string_lossy(),
        "the store stamps the read-only data dir"
    );

    settings.appearance.theme_mode = ThemeMode::Dark;
    settings.appearance.text_size_multiplier = 1.25;
    settings.editor.font = "Berkeley Mono".to_string();
    settings
        .shortcuts
        .insert("session.new".into(), "cmd+n".into());
    settings.providers.get_mut("openai").unwrap().has_key = true;
    store.save(&settings).unwrap();

    let reloaded = store.load();
    assert_eq!(reloaded.appearance.theme_mode, ThemeMode::Dark);
    assert_eq!(reloaded.appearance.text_size_multiplier, 1.25);
    assert_eq!(reloaded.editor.font, "Berkeley Mono");
    assert_eq!(reloaded.shortcuts["session.new"], "cmd+n");
    assert!(reloaded.providers["openai"].has_key);

    // Hand-editable: the file on disk is pretty-printed JSON with no secrets in it.
    let raw = std::fs::read_to_string(store.path()).unwrap();
    assert!(raw.contains("\n  \"appearance\""));
    assert!(!raw.to_lowercase().contains("apikey"));
}

#[test]
fn save_is_atomic_and_leaves_no_temp_file() {
    let dir = TempDir::new("atomic");
    let store = dir.store();
    store.save(&Settings::default()).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
}

#[test]
fn a_corrupt_file_falls_back_to_defaults_and_is_preserved() {
    let dir = TempDir::new("corrupt");
    let store = dir.store();
    std::fs::write(store.path(), "{ this is not settings").unwrap();

    let outcome = store.load_reporting();
    assert_eq!(outcome.settings.version, SETTINGS_VERSION);
    assert_eq!(outcome.settings.appearance.theme_mode, ThemeMode::System);

    let issue = outcome.issue.expect("corruption must be reported");
    assert_eq!(issue.code, "settings_invalid_json");
    let backup = issue.detail.unwrap()["backup"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        std::fs::read_to_string(&backup)
            .unwrap()
            .contains("not settings"),
        "the user's file is kept at {backup}"
    );

    // Launching again with no file at all is not an error.
    let fresh = SettingsStore::new(&dir.0).load_reporting();
    assert!(fresh.issue.is_none());
}

#[test]
fn missing_file_yields_defaults_without_writing_one() {
    let dir = TempDir::new("missing");
    let store = dir.store();
    let outcome = store.load_reporting();
    assert!(outcome.issue.is_none());
    assert!(!store.path().exists(), "load must not create the file");
    assert_eq!(outcome.settings.editor.tab_width, 4);
}
