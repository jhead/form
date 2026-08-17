//! Unpaired-surrogate removal. Port of `packages/ai/src/utils/sanitize-unicode.ts`.
//!
//! Upstream exists because a JavaScript string is a sequence of UTF-16 code
//! units and may hold a surrogate with no partner. `JSON.stringify` happily
//! emits that as a lone `\ud83d` escape, and several providers reject the
//! request outright.
//!
//! A Rust `String` is guaranteed well-formed UTF-8, so the same value cannot
//! exist in one. That makes the direct `&str -> &str` port a no-op, and there
//! is deliberately no such function here. The failure mode reappears at exactly
//! two boundaries, and there is a function for each:
//!
//! - [`sanitize_surrogates_utf16`] — text arriving as UTF-16 (the Swift/FFI
//!   boundary: `NSString` and `String` in Swift are UTF-16 backed and *can*
//!   carry unpaired surrogates).
//! - [`sanitize_surrogate_escapes`] — JSON *text* from a provider containing
//!   lone `\uD800`-`\uDFFF` escapes, which `serde_json` refuses to parse even
//!   though `JSON.parse` accepts them.

use std::borrow::Cow;

fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&unit)
}

/// Whether `units` contains a surrogate without its partner.
pub fn has_unpaired_surrogates(units: &[u16]) -> bool {
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if is_high_surrogate(unit) {
            if units.get(index + 1).copied().is_some_and(is_low_surrogate) {
                index += 2;
                continue;
            }
            return true;
        }
        if is_low_surrogate(unit) {
            return true;
        }
        index += 1;
    }
    false
}

/// Decode UTF-16, dropping any surrogate that has no partner.
///
/// Properly paired surrogates — every emoji and every other astral-plane
/// character — are preserved, matching upstream.
pub fn sanitize_surrogates_utf16(units: &[u16]) -> String {
    let mut kept: Vec<u16> = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        if is_high_surrogate(unit) {
            if units.get(index + 1).copied().is_some_and(is_low_surrogate) {
                kept.push(unit);
                kept.push(units[index + 1]);
                index += 2;
            } else {
                index += 1; // drop the unpaired high surrogate
            }
            continue;
        }
        if is_low_surrogate(unit) {
            index += 1; // drop the unpaired low surrogate
            continue;
        }
        kept.push(unit);
        index += 1;
    }
    // Every surrogate left in `kept` is paired, so this cannot fail.
    String::from_utf16_lossy(&kept)
}

/// Remove `\uXXXX` escapes that denote an unpaired surrogate from JSON *text*.
///
/// `serde_json` rejects a lone surrogate escape where `JSON.parse` accepts it
/// (yielding an ill-formed JS string). Running this over a provider payload
/// before parsing recovers the rest of the document instead of losing it all.
///
/// Returns [`Cow::Borrowed`] when there is nothing to strip, which is the
/// overwhelmingly common case.
pub fn sanitize_surrogate_escapes(json: &str) -> Cow<'_, str> {
    if !json.contains("\\u") && !json.contains("\\U") {
        return Cow::Borrowed(json);
    }

    let bytes = json.as_bytes();
    let mut out: Option<String> = None;
    let mut copied_to = 0usize;
    let mut index = 0usize;

    while index + 5 < bytes.len() + 1 {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        // A run of backslashes: only an odd-length run starts a real escape.
        let run_start = index;
        while index < bytes.len() && bytes[index] == b'\\' {
            index += 1;
        }
        let run_len = index - run_start;
        if run_len % 2 == 0 {
            continue;
        }
        let escape_start = index - 1;
        let Some(unit) = read_escape_unit(bytes, escape_start) else {
            continue;
        };

        let paired = if is_high_surrogate(unit) {
            read_escape_unit(bytes, escape_start + 6).is_some_and(is_low_surrogate)
        } else if is_low_surrogate(unit) {
            // A low surrogate is paired only when the previous escape was high.
            escape_start >= 6
                && read_escape_unit(bytes, escape_start - 6).is_some_and(is_high_surrogate)
        } else {
            true
        };

        if paired {
            index = escape_start + 6;
            continue;
        }

        let buffer = out.get_or_insert_with(|| String::with_capacity(json.len()));
        buffer.push_str(&json[copied_to..escape_start]);
        copied_to = escape_start + 6;
        index = copied_to;
    }

    match out {
        Some(mut buffer) => {
            buffer.push_str(&json[copied_to..]);
            Cow::Owned(buffer)
        }
        None => Cow::Borrowed(json),
    }
}

/// Read the code unit of a `\uXXXX` escape starting at `start`, if there is one.
fn read_escape_unit(bytes: &[u8], start: usize) -> Option<u16> {
    if start + 6 > bytes.len() || bytes[start] != b'\\' || bytes[start + 1] != b'u' {
        return None;
    }
    let digits = std::str::from_utf8(&bytes[start + 2..start + 6]).ok()?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(digits, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn preserves_valid_emoji_and_plain_text() {
        for text in ["Hello 🙈 World", "plain ascii", "accented é and 日本語", ""] {
            assert_eq!(sanitize_surrogates_utf16(&units(text)), text);
            assert!(!has_unpaired_surrogates(&units(text)));
        }
    }

    #[test]
    fn removes_an_unpaired_high_surrogate() {
        let mut input = units("Text ");
        input.push(0xD83D);
        input.extend(units(" here"));
        assert!(has_unpaired_surrogates(&input));
        assert_eq!(sanitize_surrogates_utf16(&input), "Text  here");
    }

    #[test]
    fn removes_an_unpaired_low_surrogate() {
        let mut input = units("a");
        input.push(0xDE48);
        input.extend(units("b"));
        assert!(has_unpaired_surrogates(&input));
        assert_eq!(sanitize_surrogates_utf16(&input), "ab");
    }

    #[test]
    fn keeps_a_pair_that_immediately_follows_a_stray_high_surrogate() {
        // 0xD83D alone, then the real pair for 🙈.
        let input = vec![0xD83D, 0xD83D, 0xDE48];
        assert_eq!(sanitize_surrogates_utf16(&input), "🙈");
    }

    #[test]
    fn a_trailing_high_surrogate_is_dropped() {
        let mut input = units("end");
        input.push(0xD800);
        assert_eq!(sanitize_surrogates_utf16(&input), "end");
    }

    #[test]
    fn escape_stripping_borrows_when_there_is_nothing_to_do() {
        for json in [r#"{"a":1}"#, r#"{"a":"é"}"#, r#"{"a":"🙈"}"#] {
            assert!(
                matches!(sanitize_surrogate_escapes(json), Cow::Borrowed(_)),
                "{json}"
            );
        }
    }

    #[test]
    fn strips_a_lone_surrogate_escape_so_the_payload_parses() {
        let json = r#"{"text":"hi \ud83d there"}"#;
        let cleaned = sanitize_surrogate_escapes(json);
        assert_eq!(cleaned, r#"{"text":"hi  there"}"#);
        assert!(serde_json::from_str::<serde_json::Value>(json).is_err());
        assert!(serde_json::from_str::<serde_json::Value>(&cleaned).is_ok());
    }

    #[test]
    fn keeps_a_valid_surrogate_pair_escape() {
        let json = r#"{"text":"🙈 ok"}"#;
        let cleaned = sanitize_surrogate_escapes(json);
        let parsed: serde_json::Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(parsed["text"], "🙈 ok");
    }

    #[test]
    fn strips_a_lone_low_surrogate_escape() {
        let json = r#"{"text":"a\ude48b"}"#;
        assert_eq!(sanitize_surrogate_escapes(json), r#"{"text":"ab"}"#);
    }

    #[test]
    fn an_escaped_backslash_does_not_start_an_escape() {
        // `\\u0041` is a literal backslash followed by the text "u0041".
        let json = r#"{"text":"\\ud83d"}"#;
        assert!(matches!(sanitize_surrogate_escapes(json), Cow::Borrowed(_)));
    }

    #[test]
    fn handles_consecutive_lone_surrogates() {
        let json = r#"{"t":"\ud800\ud801x"}"#;
        // The first is followed by another high surrogate, so neither is paired.
        assert_eq!(sanitize_surrogate_escapes(json), r#"{"t":"x"}"#);
    }
}
