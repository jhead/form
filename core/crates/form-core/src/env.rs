//! Loading provider credentials from a `.env` file.
//!
//! A GUI app launched from Finder inherits no shell environment, so an exported variable in
//! the user's profile never reaches it. `pi-auth` resolves keys from the process environment,
//! so this reads a `.env` and puts them there before the SDK is built.
//!
//! Values already present in the environment win: an explicitly exported key should beat a
//! file the user forgot about.

use std::path::{Path, PathBuf};

/// Provider-specific aliases people actually write, mapped to the name `pi-auth` expects.
const ALIASES: &[(&str, &str)] = &[
    ("OPENROUTER_KEY", "OPENROUTER_API_KEY"),
    ("ANTHROPIC_KEY", "ANTHROPIC_API_KEY"),
    ("OPENAI_KEY", "OPENAI_API_KEY"),
];

/// Load `.env` from `dir`, then from each ancestor, and export what it finds.
/// Returns the file that was used.
pub fn load(dir: &Path) -> Option<PathBuf> {
    // Canonicalize first: `Path::new(".").parent()` is `""`, so walking ancestors of a
    // relative path stops immediately and silently finds nothing.
    let absolute = dir
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| dir.to_path_buf()));

    let mut current = Some(absolute.as_path());
    while let Some(path) = current {
        let candidate = path.join(".env");
        if candidate.is_file() && apply(&candidate) {
            return Some(candidate);
        }
        current = path.parent();
    }
    None
}

fn apply(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_start_matches("export ").trim();
        // Strip one layer of matching quotes, which is how most .env files are written.
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);

        set_if_absent(key, value);
        for (alias, canonical) in ALIASES {
            if key == *alias {
                set_if_absent(canonical, value);
            }
        }
    }
    true
}

fn set_if_absent(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: called during core construction, before any worker thread reads the
        // environment. Rust 1.90 marks `set_var` unsafe for exactly that reason.
        unsafe { std::env::set_var(key, value) };
    }
}
