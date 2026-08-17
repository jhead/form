//! Shared truncation utilities for tool output.
//!
//! Port of `.upstream/packages/agent/src/harness/utils/truncate.ts`.
//!
//! Truncation applies two independent limits, whichever is hit first: a line
//! limit and a byte limit. Head truncation never returns a partial line; tail
//! truncation may, but only when the single last line exceeds the byte limit.
//!
//! Upstream counts UTF-8 bytes over a UTF-16 string and has to hand-roll the
//! surrogate arithmetic. Rust strings are already UTF-8, so byte counting is
//! `len()` and the surrogate handling has no analogue (a `String` cannot hold an
//! unpaired surrogate).

use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// Max characters per grep match line.
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Which limit caused truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    /// The truncated content.
    pub content: String,
    pub truncated: bool,
    /// Which limit was hit, or `None` when not truncated.
    #[serde(default)]
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    /// Complete lines in the truncated output.
    pub output_lines: usize,
    pub output_bytes: usize,
    /// Whether the last line was partially truncated (tail truncation only).
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit (head truncation only).
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl TruncationOptions {
    pub fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            max_lines,
            max_bytes,
        }
    }

    /// Byte limit only: used where a row limit already caps the output (grep, find).
    pub fn bytes_only(max_bytes: usize) -> Self {
        Self {
            max_lines: usize::MAX,
            max_bytes,
        }
    }
}

/// `content.split("\n")` with a trailing newline not counted as an extra line.
fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Format bytes as a human-readable size.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn untruncated(content: &str, options: TruncationOptions, lines: usize) -> TruncationResult {
    TruncationResult {
        content: content.to_string(),
        truncated: false,
        truncated_by: None,
        total_lines: lines,
        total_bytes: content.len(),
        output_lines: lines,
        output_bytes: content.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: options.max_lines,
        max_bytes: options.max_bytes,
    }
}

/// Truncate from the head, keeping the first N lines/bytes. Never returns a
/// partial line: if the first line alone exceeds the byte limit the content is
/// empty and `first_line_exceeds_limit` is set.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= options.max_lines && total_bytes <= options.max_bytes {
        return untruncated(content, options, total_lines);
    }

    let first_line_bytes = lines.first().map_or(0, |l| l.len());
    if first_line_bytes > options.max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines: options.max_lines,
            max_bytes: options.max_bytes,
        };
    }

    let mut output: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= options.max_lines {
            break;
        }
        // +1 for the newline that joins this line to the previous one.
        let line_bytes = line.len() + usize::from(i > 0);
        if output_bytes_count + line_bytes > options.max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output.push(line);
        output_bytes_count += line_bytes;
    }

    if output.len() >= options.max_lines && output_bytes_count <= options.max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output.join("\n");
    let final_output_bytes = output_content.len();
    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: options.max_lines,
        max_bytes: options.max_bytes,
    }
}

/// Truncate from the tail, keeping the last N lines/bytes. Suitable for shell
/// output, where the end carries the errors and final results.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= options.max_lines && total_bytes <= options.max_bytes {
        return untruncated(content, options, total_lines);
    }

    let mut output: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if output.len() >= options.max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(!output.is_empty());
        if output_bytes_count + line_bytes > options.max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Nothing fit yet and this single line is oversized: keep its tail.
            if output.is_empty() {
                let truncated_line = truncate_str_to_bytes_from_end(line, options.max_bytes);
                output_bytes_count = truncated_line.len();
                output.push(truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output.push((*line).to_string());
        output_bytes_count += line_bytes;
    }

    output.reverse();

    if output.len() >= options.max_lines && output_bytes_count <= options.max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output.join("\n");
    let final_output_bytes = output_content.len();
    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines: options.max_lines,
        max_bytes: options.max_bytes,
    }
}

/// Longest suffix of `s` that fits in `max_bytes` and starts on a char boundary.
///
/// Equivalent to upstream's `truncateStringToBytesFromEnd`, and to the
/// `Buffer.subarray(len - maxBytes)`-then-skip-continuation-bytes semantics the
/// upstream tests assert against.
fn truncate_str_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || s.is_empty() {
        return String::new();
    }
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Truncate a single line to `max_chars` characters, adding a `[truncated]` suffix.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    let char_count = line.chars().count();
    if char_count <= max_chars {
        return (line.to_string(), false);
    }
    let cut: String = line.chars().take(max_chars).collect();
    (format!("{cut}... [truncated]"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference implementation of the JS `bufferTail` helper from
    /// `test/harness/truncate.test.ts`.
    fn buffer_tail(content: &str, max_bytes: usize) -> String {
        let bytes = content.as_bytes();
        if bytes.len() <= max_bytes {
            return content.to_string();
        }
        let mut start = bytes.len() - max_bytes;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
        }
        String::from_utf8(bytes[start..].to_vec()).unwrap()
    }

    fn assert_matches_buffer_tail(input: &str) {
        for max_bytes in 0..input.len() + 5 {
            let result = truncate_tail(input, TruncationOptions::new(10, max_bytes));
            assert_eq!(
                result.content,
                buffer_tail(input, max_bytes),
                "tail mismatch input={input:?} max_bytes={max_bytes}"
            );
            assert!(
                result.content.len() <= max_bytes,
                "tail output exceeded byte limit input={input:?} max_bytes={max_bytes}"
            );
        }
    }

    #[test]
    fn counts_utf8_bytes() {
        let content = "aé🙂\nb";
        let result = truncate_head(content, TruncationOptions::new(10, 100));

        assert!(!result.truncated);
        assert_eq!(result.total_bytes, content.len());
        assert_eq!(result.output_bytes, content.len());
        assert_eq!(result.total_bytes, 9);
    }

    #[test]
    fn does_not_count_a_trailing_newline_as_an_extra_line() {
        let content = "line\nline\nline\n";
        let head = truncate_head(content, TruncationOptions::new(3, 100));
        let tail = truncate_tail(content, TruncationOptions::new(3, 100));

        assert!(!head.truncated);
        assert_eq!((head.total_lines, head.output_lines), (3, 3));
        assert!(!tail.truncated);
        assert_eq!((tail.total_lines, tail.output_lines), (3, 3));
    }

    #[test]
    fn truncates_head_on_byte_limits_without_partial_lines() {
        let result = truncate_head("éé\nabc", TruncationOptions::new(10, 4));

        assert_eq!(result.content, "éé");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(result.output_bytes, 4);
        assert!(!result.first_line_exceeds_limit);
    }

    #[test]
    fn reports_head_truncation_when_the_first_line_exceeds_the_byte_limit() {
        let result = truncate_head("éé\nabc", TruncationOptions::new(10, 3));

        assert_eq!(result.content, "");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.first_line_exceeds_limit);
    }

    #[test]
    fn truncates_tail_on_utf8_boundaries_when_only_a_partial_last_line_fits() {
        let result = truncate_tail("aé🙂b", TruncationOptions::new(10, 5));

        assert_eq!(result.content, "🙂b");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.last_line_partial);
        assert_eq!(result.output_bytes, 5);
    }

    #[test]
    fn truncates_an_oversized_single_line_with_a_trailing_newline() {
        let input = format!("{}\n", "X".repeat(300_000));
        let result = truncate_tail(&input, TruncationOptions::new(100, 1024));

        assert_eq!(result.content, "X".repeat(1024));
        assert_eq!(result.output_bytes, 1024);
        assert_eq!(result.output_lines, 1);
        assert!(result.last_line_partial);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn drops_an_oversized_trailing_character_that_cannot_fit() {
        let result = truncate_tail("abc🙂", TruncationOptions::new(10, 3));

        assert_eq!(result.content, "");
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert!(result.last_line_partial);
        assert_eq!(result.output_bytes, 0);
    }

    #[test]
    fn matches_buffer_tail_semantics_across_deterministic_fuzz_cases() {
        // Upstream's alphabet minus the lone surrogates, which cannot exist in a
        // Rust `String`; the paired ones are covered by the astral characters.
        let alphabet = [
            "a",
            "\u{7f}",
            "\u{80}",
            "é",
            "\u{7ff}",
            "\u{800}",
            "中",
            "\u{d7ff}",
            "🙂",
            "\u{e000}",
            "\u{ffff}",
            "👩\u{200d}💻",
        ];

        fn check_exhaustive(prefix: &str, depth: usize, alphabet: &[&str]) {
            assert_matches_buffer_tail(prefix);
            if depth == 0 {
                return;
            }
            for character in alphabet {
                check_exhaustive(&format!("{prefix}{character}"), depth - 1, alphabet);
            }
        }
        check_exhaustive("", 2, &alphabet);

        // Deterministic LCG, same shape as the upstream fuzz loop.
        let mut seed: u32 = 0x1234_5678;
        let mut random = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed as f64 / 4294967296.0
        };
        for _ in 0..200 {
            let mut input = String::new();
            let length = (random() * 40.0) as usize;
            for _ in 0..length {
                input.push_str(alphabet[(random() * alphabet.len() as f64) as usize]);
            }
            assert_matches_buffer_tail(&input);
        }
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(50 * 1024), "50.0KB");
        assert_eq!(format_size(60000), "58.6KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
    }

    #[test]
    fn truncates_long_lines() {
        let (text, was_truncated) = truncate_line("abcdef", 3);
        assert_eq!(text, "abc... [truncated]");
        assert!(was_truncated);

        let (text, was_truncated) = truncate_line("abc", 3);
        assert_eq!(text, "abc");
        assert!(!was_truncated);
    }
}
