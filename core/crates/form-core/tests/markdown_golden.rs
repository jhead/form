//! Golden-file tests for `form_core::markdown` (spec 05 §6).
//!
//! Every `tests/fixtures/markdown/*.md` is parsed and compared against the `*.json` beside
//! it. A fixture named `*.partial.md` is parsed with `complete: false`, which is how the
//! streaming repairs get pinned down.
//!
//! Regenerate after an intentional change:
//!
//! ```sh
//! FORM_UPDATE_GOLDEN=1 cargo test -p form-core --test markdown_golden
//! ```
//!
//! Then read the diff. These files are the contract W11 renders against; an unexplained
//! change to one of them is a bug report, not a rubber stamp.

use std::fs;
use std::path::{Path, PathBuf};

use form_core::markdown;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/markdown")
}

fn fixtures() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(fixtures_dir())
        .expect("fixtures directory")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "md").then_some(path)
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no markdown fixtures found");
    paths
}

#[test]
fn fixtures_match_their_golden_json() {
    let update = std::env::var_os("FORM_UPDATE_GOLDEN").is_some();
    let mut stale: Vec<String> = Vec::new();

    for source_path in fixtures() {
        let name = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let source = fs::read_to_string(&source_path).expect("read fixture");
        let complete = !name.ends_with(".partial.md");
        let doc = markdown::parse_streaming(&source, complete);
        let mut actual = serde_json::to_string_pretty(&doc).expect("serialize doc");
        actual.push('\n');

        let golden_path = source_path.with_extension("json");
        if update {
            fs::write(&golden_path, &actual).expect("write golden");
            continue;
        }

        let expected = fs::read_to_string(&golden_path).unwrap_or_else(|_| {
            panic!("missing golden for {name}; run with FORM_UPDATE_GOLDEN=1 to create it")
        });
        if expected != actual {
            stale.push(name);
        }
    }

    assert!(
        stale.is_empty(),
        "golden files out of date: {stale:?} — inspect the change, then rerun with \
         FORM_UPDATE_GOLDEN=1"
    );
}

/// The golden JSON is what Swift decodes, so it must also decode back into the Rust types.
#[test]
fn golden_json_round_trips() {
    for source_path in fixtures() {
        let golden_path = source_path.with_extension("json");
        let Ok(json) = fs::read_to_string(&golden_path) else {
            continue;
        };
        let doc: markdown::MarkdownDoc = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("{} did not decode: {e}", golden_path.display()));
        let again = serde_json::to_string_pretty(&doc).expect("re-serialize");
        assert_eq!(
            json.trim_end(),
            again.trim_end(),
            "{} is not stable through a decode/encode cycle",
            golden_path.display()
        );
    }
}
