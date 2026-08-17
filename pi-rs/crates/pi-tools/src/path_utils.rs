//! Path normalization shared by the execution tools.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/path-utils.ts`.

use pi_core::AbortSignal;
use unicode_normalization::UnicodeNormalization;

use crate::error::FileResult;
use crate::types::ExecutionEnv;

const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

/// Fold the exotic spaces models like to emit, and drop a leading `@` (the
/// mention prefix used by the CLI's file autocomplete).
pub fn normalize_tool_path(path: &str) -> String {
    let normalized: String = path
        .chars()
        .map(|c| match c {
            '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect();
    normalized
        .strip_prefix('@')
        .map(str::to_string)
        .unwrap_or(normalized)
}

/// Resolve a model-supplied path to an absolute path. The path need not exist.
pub async fn resolve_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    abort: Option<AbortSignal>,
) -> FileResult<String> {
    env.absolute_path(&normalize_tool_path(path), abort).await
}

/// Resolve a path for reading, trying the spellings a model is likely to get
/// wrong on macOS: screenshot names use a narrow no-break space before AM/PM,
/// HFS+ stores filenames in NFD, and typographic apostrophes are common.
///
/// Falls back to the plain resolved path when no variant exists, so the caller
/// reports the error against the path the model actually asked for.
pub async fn resolve_read_tool_path(
    env: &dyn ExecutionEnv,
    path: &str,
    abort: Option<AbortSignal>,
) -> FileResult<String> {
    let resolved = resolve_tool_path(env, path, abort.clone()).await?;
    let variants = [
        resolved.clone(),
        replace_meridiem_space(&resolved),
        resolved.nfd().collect::<String>(),
        resolved.replace('\'', "\u{2019}"),
        resolved.nfd().collect::<String>().replace('\'', "\u{2019}"),
    ];

    let mut seen: Vec<&str> = Vec::with_capacity(variants.len());
    for variant in &variants {
        if seen.contains(&variant.as_str()) {
            continue;
        }
        seen.push(variant);
        if env.exists(variant, abort.clone()).await? {
            return Ok(variant.clone());
        }
    }
    Ok(resolved)
}

/// ` AM.` / ` PM.` (any case) becomes narrow-no-break-space + AM/PM, matching
/// upstream's `/ (AM|PM)\./gi` replacement.
fn replace_meridiem_space(path: &str) -> String {
    let bytes: Vec<char> = path.chars().collect();
    let mut out = String::with_capacity(path.len());
    let mut i = 0;
    while i < bytes.len() {
        let is_meridiem = bytes[i] == ' '
            && i + 3 < bytes.len()
            && bytes[i + 3] == '.'
            && matches!(bytes[i + 1], 'A' | 'a' | 'P' | 'p')
            && matches!(bytes[i + 2], 'M' | 'm');
        if is_meridiem {
            out.push(NARROW_NO_BREAK_SPACE);
            out.push(bytes[i + 1]);
            out.push(bytes[i + 2]);
            out.push('.');
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::memory::MemoryExecutionEnv;

    #[test]
    fn strips_a_leading_mention_prefix() {
        assert_eq!(normalize_tool_path("@src/main.rs"), "src/main.rs");
        assert_eq!(normalize_tool_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn folds_unicode_spaces() {
        assert_eq!(normalize_tool_path("a\u{00A0}b"), "a b");
        assert_eq!(normalize_tool_path("a\u{2009}b"), "a b");
        assert_eq!(normalize_tool_path("a\u{3000}b"), "a b");
        // U+202F is folded here even though the read variants add it back.
        assert_eq!(normalize_tool_path("a\u{202F}b"), "a b");
    }

    #[test]
    fn rewrites_the_meridiem_space() {
        assert_eq!(
            replace_meridiem_space("Screenshot at 9.41.00 AM.png"),
            "Screenshot at 9.41.00\u{202F}AM.png"
        );
        assert_eq!(replace_meridiem_space("shot pm.png"), "shot\u{202F}pm.png");
        assert_eq!(replace_meridiem_space("plain name.png"), "plain name.png");
    }

    #[tokio::test]
    async fn resolves_relative_paths_against_the_cwd() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let resolved = resolve_tool_path(env.as_ref(), "@sub/file.txt", None)
            .await
            .unwrap();
        assert_eq!(resolved, "/work/sub/file.txt");
    }

    #[tokio::test]
    async fn read_resolution_prefers_an_existing_variant() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        env.write_text("/work/it\u{2019}s.txt", "hi").await;

        let resolved = resolve_read_tool_path(env.as_ref(), "it's.txt", None)
            .await
            .unwrap();
        assert_eq!(resolved, "/work/it\u{2019}s.txt");
    }

    #[tokio::test]
    async fn read_resolution_falls_back_to_the_plain_path() {
        let env = Arc::new(MemoryExecutionEnv::new("/work"));
        let resolved = resolve_read_tool_path(env.as_ref(), "missing.txt", None)
            .await
            .unwrap();
        assert_eq!(resolved, "/work/missing.txt");
    }
}
