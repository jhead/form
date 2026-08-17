//! Match/replace and diff computation behind the edit tool.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/edit-diff.ts`.
//!
//! The subtlety lives in [`apply_edits_to_normalized_content`]: every edit is
//! matched against the *original* content (not incrementally), an exact match is
//! tried before a fuzzy one, and when any edit needed fuzzy matching the whole
//! operation moves into fuzzy-normalized space and the result is overlaid back
//! onto the original so untouched lines keep their original bytes.
//!
//! Offsets are byte offsets here where upstream uses UTF-16 code unit offsets.
//! Every offset is produced and consumed within this module, so the two are
//! interchangeable.

use similar::{ChangeTag, TextDiff};
use unicode_normalization::UnicodeNormalization;

use crate::error::ToolError;

/// The line ending a file uses, decided by whichever appears first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

pub fn detect_line_ending(content: &str) -> LineEnding {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (Some(crlf), Some(lf)) if crlf < lf => LineEnding::Crlf,
        _ => LineEnding::Lf,
    }
}

pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => text.replace('\n', "\r\n"),
        LineEnding::Lf => text.to_string(),
    }
}

/// Normalize text for fuzzy matching: NFKC, strip per-line trailing whitespace,
/// fold smart quotes, Unicode dashes and exotic spaces to ASCII.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();
    let trimmed = nfkc
        .split('\n')
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    trimmed
        .chars()
        .map(|c| match c {
            // Smart single quotes.
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            // Smart double quotes.
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            // Hyphen, non-breaking hyphen, figure dash, en dash, em dash,
            // horizontal bar, minus sign.
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            // NBSP, the U+2002..U+200A run, narrow NBSP, medium math space,
            // ideographic space.
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Split into lines that keep their trailing newline. Upstream's
/// `/[^\n]*\n|[^\n]+/g`; `split_inclusive` produces the same segmentation.
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    content.split_inclusive('\n').collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineSpan {
    start: usize,
    end: usize,
}

/// One replacement resolved against the base content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplacement {
    pub match_index: usize,
    pub match_length: usize,
    pub new_text: String,
}

#[derive(Debug, Clone)]
struct MatchedEdit {
    edit_index: usize,
    replacement: TextReplacement,
}

fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

fn get_replacement_line_range(
    lines: &[LineSpan],
    replacement: &TextReplacement,
) -> Result<(usize, usize), ToolError> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let start_line = lines
        .iter()
        .position(|line| replacement_start >= line.start && replacement_start < line.end)
        .ok_or_else(|| ToolError::failed("Replacement range is outside the base content."))?;

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(ToolError::failed(
            "Replacement range is outside the base content.",
        ));
    }
    Ok((start_line, end_line + 1))
}

/// Apply replacements right-to-left so earlier offsets stay valid.
fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        result.replace_range(
            match_index..match_index + replacement.match_length,
            &replacement.new_text,
        );
    }
    result
}

/// Apply replacements matched against `base_content` to `original_content` while
/// preserving unchanged line blocks from the original.
///
/// `base_content` is a normalized view of the original with the same line count.
/// Each replacement is widened to the lines it touches; those lines are rewritten
/// from the normalized base and every other line is copied back verbatim. Using
/// the actual replacement ranges (not text matching) is what stops duplicate
/// normalized lines from being aligned to the wrong occurrence.
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String, ToolError> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(ToolError::failed(
            "Cannot preserve unchanged lines because the base content has a different line count.",
        ));
    }

    struct Group {
        start_line: usize,
        end_line: usize,
        replacements: Vec<TextReplacement>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut sorted = replacements.to_vec();
    sorted.sort_by_key(|r| r.match_index);
    for replacement in sorted {
        let (start_line, end_line) = get_replacement_line_range(&base_lines, &replacement)?;
        if let Some(current) = groups.last_mut() {
            if start_line < current.end_line {
                current.end_line = current.end_line.max(end_line);
                current.replacements.push(replacement);
                continue;
            }
        }
        groups.push(Group {
            start_line,
            end_line,
            replacements: vec![replacement],
        });
    }

    let mut original_line_index = 0;
    let mut result = String::new();
    for group in &groups {
        result.push_str(&original_lines[original_line_index..group.start_line].concat());

        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        ));
        original_line_index = group.end_line;
    }
    result.push_str(&original_lines[original_line_index..].concat());

    Ok(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult {
    /// Where the match starts, in `content_for_replacement`.
    pub index: Option<usize>,
    pub match_length: usize,
    /// `false` means an exact match was found.
    pub used_fuzzy_match: bool,
    /// The content replacements should be computed against: the original for an
    /// exact match, the fuzzy-normalized form for a fuzzy one.
    pub content_for_replacement: String,
}

impl FuzzyMatchResult {
    pub fn found(&self) -> bool {
        self.index.is_some()
    }
}

/// One targeted replacement requested by the caller.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    pub base_content: String,
    pub new_content: String,
}

/// Find `old_text` in `content`, exact first then fuzzy.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    if let Some(exact_index) = content.find(old_text) {
        return FuzzyMatchResult {
            index: Some(exact_index),
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    // Work entirely in normalized space for the fuzzy pass.
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    match fuzzy_content.find(&fuzzy_old_text) {
        None => FuzzyMatchResult {
            index: None,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        },
        Some(fuzzy_index) => FuzzyMatchResult {
            index: Some(fuzzy_index),
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        },
    }
}

/// Strip a UTF-8 BOM, returning the BOM (if any) and the remaining text.
pub fn strip_bom(content: &str) -> (&str, &str) {
    match content.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", content),
    }
}

fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        return 0;
    }
    fuzzy_content.matches(&fuzzy_old_text).count()
}

fn not_found_error(path: &str, edit_index: usize, total_edits: usize) -> ToolError {
    if total_edits == 1 {
        ToolError::failed(format!(
            "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
        ))
    } else {
        ToolError::failed(format!(
            "Could not find edits[{edit_index}] in {path}. The oldText must match exactly including all whitespace and newlines."
        ))
    }
}

fn duplicate_error(
    path: &str,
    edit_index: usize,
    total_edits: usize,
    occurrences: usize,
) -> ToolError {
    if total_edits == 1 {
        ToolError::failed(format!(
            "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique."
        ))
    } else {
        ToolError::failed(format!(
            "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText must be unique. Please provide more context to make it unique."
        ))
    }
}

fn empty_old_text_error(path: &str, edit_index: usize, total_edits: usize) -> ToolError {
    if total_edits == 1 {
        ToolError::failed(format!("oldText must not be empty in {path}."))
    } else {
        ToolError::failed(format!(
            "edits[{edit_index}].oldText must not be empty in {path}."
        ))
    }
}

fn no_change_error(path: &str, total_edits: usize) -> ToolError {
    if total_edits == 1 {
        ToolError::failed(format!(
            "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
        ))
    } else {
        ToolError::failed(format!(
            "No changes made to {path}. The replacements produced identical content."
        ))
    }
}

/// Apply one or more exact-text replacements to LF-normalized content.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, ToolError> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();
    let total = normalized_edits.len();

    for (i, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(empty_old_text_error(path, i, total));
        }
    }

    let used_fuzzy_match = normalized_edits
        .iter()
        .any(|edit| fuzzy_find_text(normalized_content, &edit.old_text).used_fuzzy_match);
    let replacement_base_content = if used_fuzzy_match {
        normalize_for_fuzzy_match(normalized_content)
    } else {
        normalized_content.to_string()
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::with_capacity(total);
    for (i, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        let Some(index) = match_result.index else {
            return Err(not_found_error(path, i, total));
        };

        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(duplicate_error(path, i, total, occurrences));
        }

        matched_edits.push(MatchedEdit {
            edit_index: i,
            replacement: TextReplacement {
                match_index: index,
                match_length: match_result.match_length,
                new_text: edit.new_text.clone(),
            },
        });
    }

    matched_edits.sort_by_key(|m| m.replacement.match_index);
    for window in matched_edits.windows(2) {
        let (previous, current) = (&window[0], &window[1]);
        if previous.replacement.match_index + previous.replacement.match_length
            > current.replacement.match_index
        {
            return Err(ToolError::failed(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.edit_index, current.edit_index
            )));
        }
    }

    let replacements: Vec<TextReplacement> = matched_edits
        .into_iter()
        .map(|m| m.replacement)
        .collect::<Vec<_>>();

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(
            normalized_content,
            &replacement_base_content,
            &replacements,
        )?
    } else {
        apply_replacements(&replacement_base_content, &replacements, 0)
    };

    if base_content == new_content {
        return Err(no_change_error(path, total));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

/// Generate a standard unified patch with `context_lines` of context.
pub fn generate_unified_patch(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut unified = diff.unified_diff();
    unified.context_radius(context_lines);
    unified.header(path, path);
    unified.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffString {
    pub diff: String,
    /// Line number of the first change, in the new file.
    pub first_changed_line: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Equal,
    Added,
    Removed,
}

/// Coalesce a `similar` change stream into jsdiff-style parts: runs of equal
/// lines, and for each changed region the removed lines before the added ones.
fn diff_parts(old_content: &str, new_content: &str) -> Vec<(PartKind, Vec<String>)> {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut parts: Vec<(PartKind, Vec<String>)> = Vec::new();
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => PartKind::Equal,
            ChangeTag::Insert => PartKind::Added,
            ChangeTag::Delete => PartKind::Removed,
        };
        let line = change.value().strip_suffix('\n').unwrap_or(change.value());
        match parts.last_mut() {
            Some((last_kind, lines)) if *last_kind == kind => lines.push(line.to_string()),
            _ => parts.push((kind, vec![line.to_string()])),
        }
    }
    parts
}

/// Display-oriented diff with line numbers and elided context, plus the first
/// changed line number in the new file.
pub fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> DiffString {
    let parts = diff_parts(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let max_line_num = old_content
        .split('\n')
        .count()
        .max(new_content.split('\n').count());
    let width = max_line_num.to_string().len();

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for i in 0..parts.len() {
        let (kind, raw) = &parts[i];
        match kind {
            PartKind::Added | PartKind::Removed => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for line in raw {
                    if *kind == PartKind::Added {
                        output.push(format!("+{new_line_num:>width$} {line}"));
                        new_line_num += 1;
                    } else {
                        output.push(format!("-{old_line_num:>width$} {line}"));
                        old_line_num += 1;
                    }
                }
                last_was_change = true;
            }
            PartKind::Equal => {
                let next_part_is_change =
                    parts.get(i + 1).is_some_and(|(k, _)| *k != PartKind::Equal);
                let has_leading_change = last_was_change;
                let has_trailing_change = next_part_is_change;
                let ellipsis = format!(" {:>width$} ...", "");
                macro_rules! emit_context {
                    ($lines:expr) => {
                        for line in $lines {
                            output.push(format!(" {old_line_num:>width$} {line}"));
                            old_line_num += 1;
                            new_line_num += 1;
                        }
                    };
                }

                if has_leading_change && has_trailing_change {
                    if raw.len() <= context_lines * 2 {
                        emit_context!(raw);
                    } else {
                        let skipped = raw.len() - context_lines * 2;
                        emit_context!(&raw[..context_lines]);
                        output.push(ellipsis.clone());
                        old_line_num += skipped;
                        new_line_num += skipped;
                        emit_context!(&raw[raw.len() - context_lines..]);
                    }
                } else if has_leading_change {
                    let shown = context_lines.min(raw.len());
                    let skipped = raw.len() - shown;
                    emit_context!(&raw[..shown]);
                    if skipped > 0 {
                        output.push(ellipsis.clone());
                        old_line_num += skipped;
                        new_line_num += skipped;
                    }
                } else if has_trailing_change {
                    let skipped = raw.len().saturating_sub(context_lines);
                    if skipped > 0 {
                        output.push(ellipsis.clone());
                        old_line_num += skipped;
                        new_line_num += skipped;
                    }
                    emit_context!(&raw[skipped..]);
                } else {
                    // No adjacent change: skip these context lines entirely.
                    old_line_num += raw.len();
                    new_line_num += raw.len();
                }

                last_was_change = false;
            }
        }
    }

    DiffString {
        diff: output.join("\n"),
        first_changed_line,
    }
}

/// Default context width used by the edit tool for both diff formats.
pub const DEFAULT_DIFF_CONTEXT_LINES: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old_text: &str, new_text: &str) -> Edit {
        Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }
    }

    #[test]
    fn detects_line_endings() {
        assert_eq!(detect_line_ending("a\r\nb"), LineEnding::Crlf);
        assert_eq!(detect_line_ending("a\nb\r\n"), LineEnding::Lf);
        assert_eq!(detect_line_ending("no newline"), LineEnding::Lf);
    }

    #[test]
    fn normalizes_and_restores_line_endings() {
        assert_eq!(normalize_to_lf("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Crlf), "a\r\nb");
        assert_eq!(restore_line_endings("a\nb", LineEnding::Lf), "a\nb");
    }

    #[test]
    fn strips_bom() {
        assert_eq!(strip_bom("\u{FEFF}abc"), ("\u{FEFF}", "abc"));
        assert_eq!(strip_bom("abc"), ("", "abc"));
    }

    #[test]
    fn fuzzy_normalization_folds_quotes_dashes_spaces_and_trailing_whitespace() {
        assert_eq!(normalize_for_fuzzy_match("a\u{2019}b"), "a'b");
        assert_eq!(normalize_for_fuzzy_match("\u{201C}x\u{201D}"), "\"x\"");
        assert_eq!(normalize_for_fuzzy_match("a\u{2014}b"), "a-b");
        assert_eq!(normalize_for_fuzzy_match("a\u{00A0}b"), "a b");
        assert_eq!(normalize_for_fuzzy_match("a   \nb\t"), "a\nb");
    }

    #[test]
    fn finds_exact_matches_before_fuzzy_ones() {
        let result = fuzzy_find_text("hello world", "world");
        assert_eq!(result.index, Some(6));
        assert!(!result.used_fuzzy_match);
        assert_eq!(result.match_length, 5);
    }

    #[test]
    fn falls_back_to_fuzzy_matching() {
        let result = fuzzy_find_text("const a = \u{2018}x\u{2019};", "const a = 'x';");
        assert!(result.found());
        assert!(result.used_fuzzy_match);
        assert_eq!(result.content_for_replacement, "const a = 'x';");
    }

    #[test]
    fn reports_no_match() {
        let result = fuzzy_find_text("abc", "zzz");
        assert!(!result.found());
        assert_eq!(result.match_length, 0);
    }

    #[test]
    fn applies_disjoint_edits_against_the_original() {
        let result = apply_edits_to_normalized_content(
            "alpha\nbeta\ngamma\ndelta\n",
            &[edit("alpha\n", "ALPHA\n"), edit("gamma\n", "GAMMA\n")],
            "edit.txt",
        )
        .unwrap();
        assert_eq!(result.new_content, "ALPHA\nbeta\nGAMMA\ndelta\n");
        assert_eq!(result.base_content, "alpha\nbeta\ngamma\ndelta\n");
    }

    #[test]
    fn applies_edits_in_any_order() {
        // The second edit targets an earlier offset; replacements must still land.
        let result = apply_edits_to_normalized_content(
            "one\ntwo\nthree\n",
            &[edit("three", "THREE"), edit("one", "ONE")],
            "f.txt",
        )
        .unwrap();
        assert_eq!(result.new_content, "ONE\ntwo\nTHREE\n");
    }

    #[test]
    fn rejects_overlapping_edits() {
        let error = apply_edits_to_normalized_content(
            "one\ntwo\nthree\n",
            &[
                edit("one\ntwo\n", "ONE\nTWO\n"),
                edit("two\nthree\n", "X\n"),
            ],
            "edit.txt",
        )
        .unwrap_err();
        assert!(error.message().contains("overlap"), "{}", error.message());
    }

    #[test]
    fn rejects_missing_text() {
        let error =
            apply_edits_to_normalized_content("foo foo foo", &[edit("bar", "baz")], "edit.txt")
                .unwrap_err();
        assert!(error.message().contains("Could not find the exact text"));
    }

    #[test]
    fn rejects_duplicate_text() {
        let error =
            apply_edits_to_normalized_content("foo foo foo", &[edit("foo", "bar")], "edit.txt")
                .unwrap_err();
        assert!(
            error.message().contains("Found 3 occurrences"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn indexes_errors_when_there_are_several_edits() {
        let error = apply_edits_to_normalized_content(
            "alpha\nbeta\n",
            &[edit("alpha", "ALPHA"), edit("zzz", "x")],
            "edit.txt",
        )
        .unwrap_err();
        assert!(
            error.message().contains("Could not find edits[1]"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn rejects_empty_old_text() {
        let error =
            apply_edits_to_normalized_content("alpha\n", &[edit("", "x")], "edit.txt").unwrap_err();
        assert_eq!(error.message(), "oldText must not be empty in edit.txt.");
    }

    #[test]
    fn rejects_no_op_replacements() {
        let error =
            apply_edits_to_normalized_content("alpha\n", &[edit("alpha", "alpha")], "f.txt")
                .unwrap_err();
        assert!(error.message().starts_with("No changes made to f.txt."));
    }

    #[test]
    fn fuzzy_edits_preserve_unchanged_original_lines() {
        // Line 1 has a smart quote and trailing whitespace that the edit does not
        // target; it must survive byte-for-byte.
        let original = "let a = \u{2018}keep\u{2019};   \nlet b = 'change me';\n";
        let result = apply_edits_to_normalized_content(
            original,
            &[edit("let b = 'change me';", "let b = 'changed';")],
            "f.ts",
        )
        .unwrap();
        assert_eq!(
            result.new_content,
            "let a = \u{2018}keep\u{2019};   \nlet b = 'changed';\n"
        );
    }

    #[test]
    fn fuzzy_edits_rewrite_only_the_touched_lines() {
        let original = "a = \u{201C}x\u{201D}\nb = 1\n";
        let result =
            apply_edits_to_normalized_content(original, &[edit("a = \"x\"", "a = \"y\"")], "f.txt")
                .unwrap();
        // The matched line is rewritten from normalized space, the rest is original.
        assert_eq!(result.new_content, "a = \"y\"\nb = 1\n");
    }

    #[test]
    fn preserving_overlay_rejects_a_line_count_mismatch() {
        let error = apply_replacements_preserving_unchanged_lines(
            "a\nb\n",
            "a\n",
            &[TextReplacement {
                match_index: 0,
                match_length: 1,
                new_text: "A".into(),
            }],
        )
        .unwrap_err();
        assert!(error.message().contains("different line count"));
    }

    #[test]
    fn generates_a_unified_patch_that_describes_the_change() {
        let patch = generate_unified_patch(
            "edit.txt",
            "alpha\nbeta\n",
            "ALPHA\nbeta\n",
            DEFAULT_DIFF_CONTEXT_LINES,
        );
        assert!(patch.starts_with("--- edit.txt\n+++ edit.txt\n"), "{patch}");
        assert!(patch.contains("-alpha"));
        assert!(patch.contains("+ALPHA"));
    }

    #[test]
    fn generates_a_display_diff_with_line_numbers() {
        let result = generate_diff_string("alpha\nbeta\n", "ALPHA\nbeta\n", 4);
        assert_eq!(result.first_changed_line, Some(1));
        let lines: Vec<&str> = result.diff.lines().collect();
        assert_eq!(lines[0], "-1 alpha");
        assert_eq!(lines[1], "+1 ALPHA");
        assert_eq!(lines[2], " 2 beta");
    }

    #[test]
    fn elides_long_unchanged_regions() {
        let mut old_lines: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
        let new_lines = {
            let mut lines = old_lines.clone();
            lines[0] = "CHANGED".to_string();
            lines[29] = "CHANGED END".to_string();
            lines
        };
        old_lines.push(String::new());
        let old = old_lines.join("\n");
        let new = format!("{}\n", new_lines.join("\n"));

        let result = generate_diff_string(&old, &new, 4);
        assert_eq!(result.first_changed_line, Some(1));
        assert!(result.diff.contains("..."), "{}", result.diff);
        // Context is capped at four lines on each side of the elision.
        assert!(result.diff.contains(" 2 line 2"));
        assert!(result.diff.contains(" 5 line 5"));
        assert!(!result.diff.contains(" 6 line 6"));
        assert!(result.diff.contains("-30 line 30"));
        assert!(result.diff.contains("+30 CHANGED END"));
    }

    #[test]
    fn reports_no_first_changed_line_for_identical_content() {
        let result = generate_diff_string("same\n", "same\n", 4);
        assert_eq!(result.first_changed_line, None);
        assert_eq!(result.diff, "");
    }
}
