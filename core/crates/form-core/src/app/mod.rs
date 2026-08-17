//! Sessions, groups, entries, search, workspace confinement, attachments.
//!
//! **Owner: W1** (`docs/specs/01-core-domain.md`).
//!
//! [`Store`] is the single source of truth: the harness (W2) appends to it, the stats engine
//! (W3) reads its `turns` and `tool_invocations` tables, and the FFI (W6) exposes it. It is
//! SQLite rather than a log of JSON files because the Home dashboard needs grouped
//! aggregates over months and `⌘K` needs full-text search — one query each here, a full scan
//! otherwise.

pub mod search;
pub mod seed;
pub mod store;
pub mod workspace;

#[cfg(test)]
mod tests;

pub use search::SearchScope;
pub use seed::{seed, seed_if_empty, DEFAULT_SEED};
pub use store::{
    AddAttachment, AttachmentSource, Store, StoreOptions, ToolInvocationRecord, TurnRecord,
    MAX_ATTACHMENT_BYTES,
};
pub use workspace::resolve_in_workspace;

use crate::protocol::{ModelRef, ThinkingLevel};

/// The title shown until the first user message arrives (F2.6).
pub const UNTITLED: &str = "New chat";

/// Longest auto-derived title, including the ellipsis.
const TITLE_MAX_CHARS: usize = 60;

/// Derive a session title from a user message (F2.6): first non-empty line, whitespace
/// collapsed, trailing punctuation stripped, sentence-cased, and truncated on a word
/// boundary.
pub fn derive_title(text: &str) -> String {
    let first_line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let stripped = collapsed
        .trim_end_matches(['.', ',', ':', ';', '!', '?', '—', '-', '…', '"', '\'', '`'])
        .trim_end();

    let truncated = truncate_on_word(stripped, TITLE_MAX_CHARS);
    sentence_case(&truncated)
}

fn truncate_on_word(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars - 1).collect();
    // Back off to the last word boundary unless that throws away most of the title.
    let cut = match head.rfind(char::is_whitespace) {
        Some(i) if i >= max_chars / 2 => &head[..i],
        _ => &head[..],
    };
    format!("{}…", cut.trim_end())
}

/// Uppercase the first character only. Lowercasing the rest would eat `SQLite`, `FTS5` and
/// every other identifier people put in a first message.
fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn default_model_ref() -> ModelRef {
    ModelRef {
        provider_id: "anthropic".to_string(),
        model_id: "claude-opus-5".to_string(),
        thinking_level: ThinkingLevel::High,
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn takes_the_first_non_empty_line() {
        assert_eq!(
            derive_title("\n\n  add a health check endpoint\nand a test"),
            "Add a health check endpoint"
        );
    }

    #[test]
    fn collapses_whitespace_and_strips_trailing_punctuation() {
        assert_eq!(
            derive_title("fix   the\tflaky   test!!!"),
            "Fix the flaky test"
        );
        assert_eq!(derive_title("what is Pin?"), "What is Pin");
    }

    #[test]
    fn preserves_interior_capitalization() {
        assert_eq!(
            derive_title("migrate to SQLite with FTS5"),
            "Migrate to SQLite with FTS5"
        );
    }

    #[test]
    fn truncates_on_a_word_boundary_within_the_limit() {
        let long = "please refactor the notification pipeline so that email push and webhook \
                    share one retry path";
        let title = derive_title(long);
        assert!(title.chars().count() <= TITLE_MAX_CHARS, "{title}");
        assert!(title.ends_with('…'), "{title}");
        assert!(!title.contains("  "));
        // The cut lands between words, not mid-word (modulo the sentence-cased first letter).
        let head = title.trim_end_matches('…').trim_end().to_lowercase();
        assert!(
            long.starts_with(&head),
            "{head:?} is not a prefix of the message"
        );
    }

    #[test]
    fn a_single_long_word_is_cut_hard_rather_than_dropped() {
        let title = derive_title(&"x".repeat(200));
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn empty_and_whitespace_only_input_derives_nothing() {
        assert_eq!(derive_title(""), "");
        assert_eq!(derive_title("   \n\t "), "");
        assert_eq!(derive_title("..."), "");
    }

    #[test]
    fn handles_multibyte_input_without_panicking() {
        assert_eq!(derive_title("привет мир"), "Привет мир");
        assert_eq!(derive_title("🚀 ship it"), "🚀 ship it");
    }
}
