//! Partial / streaming JSON parsing. Port of `packages/ai/src/utils/json-parse.ts`.
//!
//! Tool-call arguments arrive from providers as a sequence of text fragments.
//! Adapters accumulate those fragments and, on every delta, need a *usable*
//! object for the `toolcall_delta` event even though the accumulated text is
//! almost never valid JSON yet. That is what [`parse_streaming_json`] does.
//!
//! Upstream layers three things:
//!
//! 1. [`repair_json`] — fixes the two malformations models actually emit inside
//!    string literals: raw control characters, and backslashes that do not begin
//!    a valid JSON escape.
//! 2. [`parse_json_with_repair`] — strict parse, falling back to a repaired parse.
//! 3. [`parse_streaming_json`] — strict → repaired → partial → repaired-partial,
//!    finally an empty object. Never fails.
//!
//! Upstream gets step 3's partial parser from the `partial-json` npm package.
//! There is no equivalent crate, so [`parse_partial_json`] reimplements that
//! parser's algorithm, including its quirks (see the notes on [`Allow`] and on
//! the individual parse steps), because adapters' observable output depends on
//! exactly which prefixes it accepts and what it drops.

use serde_json::{Map, Value};

/// Escape characters that may legally follow a backslash inside a JSON string.
const VALID_JSON_ESCAPES: [char; 8] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't'];

fn escape_control_character(ch: char) -> String {
    match ch {
        '\u{8}' => "\\b".to_string(),
        '\u{c}' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        other => format!("\\u{:04x}", other as u32),
    }
}

/// Repair malformed JSON string literals by
/// - escaping raw control characters inside strings, and
/// - doubling backslashes that do not introduce a valid escape.
///
/// Everything outside a string literal is passed through untouched, so this is
/// safe to run on text that is merely *incomplete* rather than malformed.
pub fn repair_json(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }

        if ch == '\\' {
            let Some(&next) = chars.get(index + 1) else {
                // Trailing backslash: emit a literal escaped backslash.
                repaired.push_str("\\\\");
                index += 1;
                continue;
            };

            if next == 'u' {
                let digits: String = chars
                    .get(index + 2..index + 6)
                    .map(|s| s.iter().collect())
                    .unwrap_or_default();
                if digits.len() == 4 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
                    repaired.push('\\');
                    repaired.push('u');
                    repaired.push_str(&digits);
                    index += 6;
                    continue;
                }
            }

            // Note: upstream's valid-escape set contains `u`, so a `\u` with bad
            // digits falls through to here and is emitted as-is rather than
            // being escaped. Kept for fidelity.
            if VALID_JSON_ESCAPES.contains(&next) || next == 'u' {
                repaired.push('\\');
                repaired.push(next);
                index += 2;
                continue;
            }

            repaired.push_str("\\\\");
            index += 1;
            continue;
        }

        if (ch as u32) <= 0x1f {
            repaired.push_str(&escape_control_character(ch));
        } else {
            repaired.push(ch);
        }
        index += 1;
    }

    repaired
}

/// Strict parse, retried once against [`repair_json`] output.
///
/// Returns the *original* parse error when repair changed nothing, matching
/// upstream, so callers see the real syntax error rather than a repaired one.
pub fn parse_json_with_repair(json: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str(json) {
        Ok(value) => Ok(value),
        Err(err) => {
            let repaired = repair_json(json);
            if repaired != json {
                serde_json::from_str(&repaired)
            } else {
                Err(err)
            }
        }
    }
}

/// Parse possibly-incomplete JSON produced mid-stream. Never fails.
///
/// Falls back through strict → repaired → partial → repaired-partial, and
/// finally to `null` when nothing parses. Prefer
/// [`parse_streaming_json_object`] for tool-call arguments.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(text) = partial_json else {
        return Value::Object(Map::new());
    };
    if text.trim().is_empty() {
        return Value::Object(Map::new());
    }

    if let Ok(value) = parse_json_with_repair(text) {
        return value;
    }
    if let Ok(value) = parse_partial_json(text, Allow::ALL) {
        return value;
    }
    if let Ok(value) = parse_partial_json(&repair_json(text), Allow::ALL) {
        return value;
    }
    Value::Object(Map::new())
}

/// [`parse_streaming_json`] narrowed to the object shape tool-call arguments
/// always have. A non-object result (a bare number, a truncated array, …)
/// becomes an empty map, which is what upstream's `as T` cast effectively means
/// for every caller in the adapter tree.
pub fn parse_streaming_json_object(partial_json: Option<&str>) -> Map<String, Value> {
    match parse_streaming_json(partial_json) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Which incomplete constructs [`parse_partial_json`] is allowed to complete.
///
/// Mirrors the `partial-json` package's bit flags. Upstream always parses with
/// [`Allow::ALL`]; the finer flags exist so the port can be tested against the
/// same matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allow(u32);

impl Allow {
    pub const NONE: Allow = Allow(0);
    pub const STR: Allow = Allow(0b0_0000_0001);
    pub const NUM: Allow = Allow(0b0_0000_0010);
    pub const ARR: Allow = Allow(0b0_0000_0100);
    pub const OBJ: Allow = Allow(0b0_0000_1000);
    pub const NULL: Allow = Allow(0b0_0001_0000);
    pub const BOOL: Allow = Allow(0b0_0010_0000);
    pub const NAN: Allow = Allow(0b0_0100_0000);
    pub const INFINITY: Allow = Allow(0b0_1000_0000);
    pub const NEG_INFINITY: Allow = Allow(0b1_0000_0000);

    pub const INF: Allow = Allow(Allow::INFINITY.0 | Allow::NEG_INFINITY.0);
    pub const SPECIAL: Allow = Allow(Allow::NULL.0 | Allow::BOOL.0 | Allow::INF.0 | Allow::NAN.0);
    pub const ATOM: Allow = Allow(Allow::STR.0 | Allow::NUM.0 | Allow::SPECIAL.0);
    pub const COLLECTION: Allow = Allow(Allow::ARR.0 | Allow::OBJ.0);
    pub const ALL: Allow = Allow(Allow::ATOM.0 | Allow::COLLECTION.0);

    pub fn contains(self, other: Allow) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Allow {
    type Output = Allow;
    fn bitor(self, rhs: Allow) -> Allow {
        Allow(self.0 | rhs.0)
    }
}

impl Default for Allow {
    fn default() -> Self {
        Allow::ALL
    }
}

/// Why a partial parse could not produce a value at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PartialJsonError {
    /// The input is a valid *prefix* but the disallowed construct at the cut
    /// point means no value can be produced.
    #[error("{message} at position {position}")]
    Partial { message: String, position: usize },
    /// The input is not a valid prefix of any JSON document.
    #[error("{message} at position {position}")]
    Malformed { message: String, position: usize },
}

/// Parse a prefix of a JSON document, completing whatever `allow` permits.
///
/// This is a direct port of the `partial-json` package's recursive-descent
/// parser. It is deliberately lenient and, like the original, does **not**
/// reject trailing garbage after the first complete value.
///
/// `NaN` and `±Infinity` are recognised (so parsing continues correctly past
/// them) but materialise as [`Value::Null`], because `serde_json::Value` has no
/// representation for them.
pub fn parse_partial_json(json: &str, allow: Allow) -> Result<Value, PartialJsonError> {
    let chars: Vec<char> = json.chars().collect();
    let mut parser = PartialParser {
        source: json,
        chars: &chars,
        index: 0,
        allow,
    };
    parser.parse_any()
}

struct PartialParser<'a> {
    /// Original text, needed for the whole-string `lastIndexOf("e")` quirk.
    source: &'a str,
    chars: &'a [char],
    index: usize,
    allow: Allow,
}

/// `String.prototype.substring`: clamps both ends into range and swaps them
/// when `start > end`. The number parser relies on the swap for `lastIndexOf`
/// misses, so it has to be reproduced rather than "fixed".
fn js_substring(chars: &[char], start: isize, end: isize) -> String {
    let len = chars.len() as isize;
    let a = start.clamp(0, len);
    let b = end.clamp(0, len);
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    chars[lo as usize..hi as usize].iter().collect()
}

impl PartialParser<'_> {
    fn len(&self) -> usize {
        self.chars.len()
    }

    fn at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    fn partial<T>(&self, message: &str) -> Result<T, PartialJsonError> {
        Err(PartialJsonError::Partial {
            message: message.to_string(),
            position: self.index,
        })
    }

    fn malformed<T>(&self, message: &str) -> Result<T, PartialJsonError> {
        Err(PartialJsonError::Malformed {
            message: message.to_string(),
            position: self.index,
        })
    }

    fn skip_blank(&mut self) {
        while let Some(ch) = self.at(self.index) {
            if matches!(ch, ' ' | '\n' | '\r' | '\t') {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    /// `substring(index, index + n) === literal`, or — when `allow` permits and
    /// the input ends inside the literal — a prefix match.
    fn match_literal(&self, literal: &str, flag: Allow) -> bool {
        let n = literal.chars().count();
        if js_substring(self.chars, self.index as isize, (self.index + n) as isize) == literal {
            return true;
        }
        if !self.allow.contains(flag) {
            return false;
        }
        let remaining = self.len().saturating_sub(self.index);
        remaining < n
            && literal.starts_with(&js_substring(
                self.chars,
                self.index as isize,
                self.len() as isize,
            ))
    }

    fn parse_any(&mut self) -> Result<Value, PartialJsonError> {
        self.skip_blank();
        if self.index >= self.len() {
            return self.partial("Unexpected end of input");
        }
        match self.at(self.index) {
            Some('"') => return self.parse_str(),
            Some('{') => return self.parse_obj(),
            Some('[') => return self.parse_arr(),
            _ => {}
        }

        if self.match_literal("null", Allow::NULL) {
            self.index += 4;
            return Ok(Value::Null);
        }
        if self.match_literal("true", Allow::BOOL) {
            self.index += 4;
            return Ok(Value::Bool(true));
        }
        if self.match_literal("false", Allow::BOOL) {
            self.index += 5;
            return Ok(Value::Bool(false));
        }
        if self.match_literal("Infinity", Allow::INFINITY) {
            self.index += 8;
            return Ok(Value::Null);
        }
        // Upstream additionally requires more than one remaining char here so a
        // bare "-" is not read as the start of -Infinity.
        if js_substring(self.chars, self.index as isize, (self.index + 9) as isize) == "-Infinity"
            || (self.allow.contains(Allow::NEG_INFINITY)
                && self.len().saturating_sub(self.index) > 1
                && self.len().saturating_sub(self.index) < 9
                && "-Infinity".starts_with(&js_substring(
                    self.chars,
                    self.index as isize,
                    self.len() as isize,
                )))
        {
            self.index += 9;
            return Ok(Value::Null);
        }
        if self.match_literal("NaN", Allow::NAN) {
            self.index += 3;
            return Ok(Value::Null);
        }

        self.parse_num()
    }

    fn parse_str(&mut self) -> Result<Value, PartialJsonError> {
        let start = self.index;
        let mut escape = false;
        self.index += 1; // skip the opening quote

        while self.index < self.len()
            && (self.at(self.index) != Some('"')
                || (escape && self.at(self.index - 1) == Some('\\')))
        {
            escape = if self.at(self.index) == Some('\\') {
                !escape
            } else {
                false
            };
            self.index += 1;
        }

        if self.at(self.index) == Some('"') {
            self.index += 1;
            let text = js_substring(self.chars, start as isize, self.index as isize);
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                return Ok(value);
            }
            // Hardening beyond upstream: a *complete* but malformed literal
            // (raw newline, Windows path escape) otherwise aborts the enclosing
            // object and discards every key parsed before it. Upstream's
            // `repairJson` pass is meant to cover this but is unreachable from
            // `parseStreamingJson`, because the unrepaired partial parse it
            // tries first swallows the failure instead of throwing.
            if let Ok(value) = serde_json::from_str::<Value>(&repair_json(&text)) {
                return Ok(value);
            }
            return self.malformed("Invalid string literal");
        }

        if self.allow.contains(Allow::STR) {
            // Close the truncated literal where the input ran out.
            let closed = format!(
                "{}\"",
                js_substring(self.chars, start as isize, self.index as isize)
            );
            if let Ok(value) = serde_json::from_str::<Value>(&closed) {
                return Ok(value);
            }
            if let Ok(value) = serde_json::from_str::<Value>(&repair_json(&closed)) {
                return Ok(value);
            }
            // The cut landed inside an escape sequence (`…\` or `…\u00`), so
            // retry from the last backslash. Upstream stops here.
            let last_backslash = self.chars[start..self.index]
                .iter()
                .rposition(|&c| c == '\\')
                .map(|i| (start + i) as isize)
                .unwrap_or(-1);
            if last_backslash > start as isize {
                let trimmed = format!(
                    "{}\"",
                    js_substring(self.chars, start as isize, last_backslash)
                );
                if let Ok(value) = serde_json::from_str::<Value>(&trimmed) {
                    return Ok(value);
                }
            }
            // Never fail a literal that has *some* parseable prefix, so a delta
            // can still surface partial arguments.
            let mut i = self.index as isize - 1;
            while i > start as isize {
                let candidate = format!("{}\"", js_substring(self.chars, start as isize, i));
                if let Ok(value) = serde_json::from_str::<Value>(&candidate) {
                    return Ok(value);
                }
                i -= 1;
            }
        }

        self.partial("Unterminated string literal")
    }

    fn parse_obj(&mut self) -> Result<Value, PartialJsonError> {
        self.index += 1; // skip `{`
        self.skip_blank();
        let mut obj = Map::new();

        let result = (|| -> Result<(), PartialJsonError> {
            while self.at(self.index) != Some('}') {
                self.skip_blank();
                if self.index >= self.len() && self.allow.contains(Allow::OBJ) {
                    return Err(PartialJsonError::Partial {
                        message: "__return_partial__".to_string(),
                        position: self.index,
                    });
                }
                let key = match self.parse_str()? {
                    Value::String(key) => key,
                    other => other.to_string(),
                };
                self.skip_blank();
                self.index += 1; // skip `:`
                match self.parse_any() {
                    Ok(value) => {
                        obj.insert(key, value);
                    }
                    Err(err) => {
                        if self.allow.contains(Allow::OBJ) {
                            return Err(PartialJsonError::Partial {
                                message: "__return_partial__".to_string(),
                                position: self.index,
                            });
                        }
                        return Err(err);
                    }
                }
                self.skip_blank();
                if self.at(self.index) == Some(',') {
                    self.index += 1;
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.index += 1; // skip `}`
                Ok(Value::Object(obj))
            }
            Err(err) => {
                if self.allow.contains(Allow::OBJ) {
                    Ok(Value::Object(obj))
                } else if matches!(err, PartialJsonError::Malformed { .. }) {
                    Err(err)
                } else {
                    self.partial("Expected '}' at end of object")
                }
            }
        }
    }

    fn parse_arr(&mut self) -> Result<Value, PartialJsonError> {
        self.index += 1; // skip `[`
        let mut arr = Vec::new();

        let result = (|| -> Result<(), PartialJsonError> {
            while self.at(self.index) != Some(']') {
                arr.push(self.parse_any()?);
                self.skip_blank();
                if self.at(self.index) == Some(',') {
                    self.index += 1;
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.index += 1; // skip `]`
                Ok(Value::Array(arr))
            }
            Err(err) => {
                if self.allow.contains(Allow::ARR) {
                    Ok(Value::Array(arr))
                } else if matches!(err, PartialJsonError::Malformed { .. }) {
                    Err(err)
                } else {
                    self.partial("Expected ']' at end of array")
                }
            }
        }
    }

    fn parse_num(&mut self) -> Result<Value, PartialJsonError> {
        // `lastIndexOf("e")` is taken over the *whole* document upstream, not
        // over the number's slice. Reproduced verbatim.
        let last_e = self
            .chars
            .iter()
            .rposition(|&c| c == 'e')
            .map(|i| i as isize)
            .unwrap_or(-1);

        if self.index == 0 {
            if self.source == "-" {
                return self.malformed("Not sure what '-' is");
            }
            if let Ok(value) = serde_json::from_str::<Value>(self.source) {
                return Ok(value);
            }
            if self.allow.contains(Allow::NUM) {
                let trimmed = js_substring(self.chars, 0, last_e);
                if let Ok(value) = serde_json::from_str::<Value>(&trimmed) {
                    return Ok(value);
                }
            }
            return self.malformed("Unexpected token");
        }

        let start = self.index;
        if self.at(self.index) == Some('-') {
            self.index += 1;
        }
        while let Some(ch) = self.at(self.index) {
            if matches!(ch, ',' | ']' | '}') {
                break;
            }
            self.index += 1;
        }

        if self.index == self.len() && !self.allow.contains(Allow::NUM) {
            return self.partial("Unterminated number literal");
        }

        let slice = js_substring(self.chars, start as isize, self.index as isize);
        if let Ok(value) = serde_json::from_str::<Value>(&slice) {
            return Ok(value);
        }
        if slice == "-" {
            return self.malformed("Not sure what '-' is");
        }
        let trimmed = js_substring(self.chars, start as isize, last_e);
        match serde_json::from_str::<Value>(&trimmed) {
            Ok(value) => Ok(value),
            Err(err) => self.malformed(&err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn partial(text: &str) -> Value {
        parse_partial_json(text, Allow::ALL).unwrap()
    }

    // --- repair_json -----------------------------------------------------

    #[test]
    fn repairs_raw_control_characters_inside_strings() {
        assert_eq!(
            repair_json("{\"a\":\"line\nbreak\"}"),
            r#"{"a":"line\nbreak"}"#
        );
        assert_eq!(repair_json("{\"a\":\"tab\there\"}"), r#"{"a":"tab\there"}"#);
        assert_eq!(repair_json("{\"a\":\"\u{1}\"}"), r#"{"a":"\u0001"}"#);
    }

    #[test]
    fn leaves_control_characters_outside_strings_alone() {
        // Whitespace between tokens is legal JSON; only string interiors change.
        assert_eq!(repair_json("{\n  \"a\": 1\n}"), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn doubles_backslashes_before_invalid_escapes() {
        // Windows paths are the common real case: `\d` is not a JSON escape.
        assert_eq!(repair_json(r#"{"p":"C:\dir"}"#), r#"{"p":"C:\\dir"}"#);
        let repaired = repair_json(r#"{"p":"C:\dir"}"#);
        assert_eq!(
            serde_json::from_str::<Value>(&repaired).unwrap(),
            json!({ "p": "C:\\dir" })
        );
    }

    #[test]
    fn preserves_valid_escapes_and_unicode() {
        assert_eq!(repair_json(r#""a\nb\u00e9c\\d""#), r#""a\nb\u00e9c\\d""#);
        assert_eq!(repair_json(r#""quote\"inside""#), r#""quote\"inside""#);
    }

    #[test]
    fn escapes_a_dangling_trailing_backslash() {
        assert_eq!(repair_json(r#"{"a":"x\"#), r#"{"a":"x\\"#);
    }

    #[test]
    fn round_trips_valid_json_unchanged() {
        let src = r#"{"a":[1,2,{"b":"c"}],"d":null,"e":true}"#;
        assert_eq!(repair_json(src), src);
    }

    // --- parse_json_with_repair -----------------------------------------

    #[test]
    fn repair_parse_recovers_control_characters() {
        let value = parse_json_with_repair("{\"a\":\"one\ntwo\"}").unwrap();
        assert_eq!(value, json!({ "a": "one\ntwo" }));
    }

    #[test]
    fn repair_parse_reports_the_original_error_when_repair_is_a_no_op() {
        // Nothing in `{` is repairable, so the original syntax error surfaces.
        assert!(parse_json_with_repair("{").is_err());
    }

    // --- partial parsing --------------------------------------------------

    #[test]
    fn completes_truncated_strings_arrays_and_objects() {
        assert_eq!(
            partial(r#"{"a": [1, 2, 3, "abc"#),
            json!({ "a": [1, 2, 3, "abc"] })
        );
        assert_eq!(partial(r#"["a", "b"#), json!(["a", "b"]));
        assert_eq!(partial(r#"{"a": 1, "b""#), json!({ "a": 1 }));
        assert_eq!(partial(r#"{"a": 1, "b": "#), json!({ "a": 1 }));
        assert_eq!(partial("{"), json!({}));
        assert_eq!(partial("["), json!([]));
    }

    #[test]
    fn completes_truncated_literals() {
        assert_eq!(partial("tru"), json!(true));
        assert_eq!(partial("fal"), json!(false));
        assert_eq!(partial("nul"), Value::Null);
        assert_eq!(partial(r#"{"a": tr"#), json!({ "a": true }));
    }

    #[test]
    fn drops_a_dangling_escape_at_the_cut_point() {
        // `"ab\` cannot be closed with a quote, so the parser walks back to `"ab`.
        assert_eq!(partial(r#"{"a": "ab\"#), json!({ "a": "ab" }));
        assert_eq!(partial(r#"{"a": "ab\u00"#), json!({ "a": "ab" }));
    }

    #[test]
    fn keeps_completed_keys_when_a_later_value_is_truncated() {
        let text = r#"{"command":"ls -la","timeout":3000,"description":"List fil"#;
        assert_eq!(
            partial(text),
            json!({ "command": "ls -la", "timeout": 3000, "description": "List fil" })
        );
    }

    #[test]
    fn parses_nested_partial_structures() {
        let text = r#"{"edits":[{"old":"foo","new":"bar"},{"old":"baz""#;
        assert_eq!(
            partial(text),
            json!({ "edits": [{ "old": "foo", "new": "bar" }, { "old": "baz" }] })
        );
    }

    #[test]
    fn respects_disallowed_flags() {
        // With STR completion off, an unterminated string is a hard failure and
        // the enclosing object still yields what it had.
        assert_eq!(
            parse_partial_json(r#"{"a": 1, "b": "xy"#, Allow::OBJ).unwrap(),
            json!({ "a": 1 })
        );
        assert!(parse_partial_json(r#""xy"#, Allow::NONE).is_err());
    }

    #[test]
    fn parses_complete_documents_identically_to_serde() {
        let src = r#"{"a":[1,2.5,-3,true,null,"s"],"b":{"c":1e3}}"#;
        assert_eq!(partial(src), serde_json::from_str::<Value>(src).unwrap());
    }

    #[test]
    fn special_numeric_literals_become_null() {
        assert_eq!(partial(r#"{"a": NaN}"#), json!({ "a": null }));
        assert_eq!(partial(r#"{"a": -Infinity}"#), json!({ "a": null }));
    }

    // --- parse_streaming_json --------------------------------------------

    #[test]
    fn streaming_empty_inputs_yield_an_empty_object() {
        assert_eq!(parse_streaming_json(None), json!({}));
        assert_eq!(parse_streaming_json(Some("")), json!({}));
        assert_eq!(parse_streaming_json(Some("   \n ")), json!({}));
    }

    #[test]
    fn streaming_prefers_the_strict_parse() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"path":"/tmp/x","limit":10}"#)),
            json!({ "path": "/tmp/x", "limit": 10 })
        );
    }

    #[test]
    fn streaming_uses_repair_before_partial() {
        // Valid structure, invalid escape: repair alone fixes it, so nothing is dropped.
        assert_eq!(
            parse_streaming_json(Some(r#"{"path":"C:\Users\me"}"#)),
            json!({ "path": "C:\\Users\\me" })
        );
    }

    /// Input that is both malformed *and* truncated. Upstream drops the whole
    /// object here (its repaired-partial branch is unreachable — see the note
    /// in `parse_str`); this port repairs per-literal instead, which is the
    /// behaviour that matters for `toolcall_delta`.
    #[test]
    fn streaming_recovers_malformed_and_truncated_literals() {
        assert_eq!(
            parse_streaming_json(Some("{\"path\":\"C:\\Users\\me\",\"body\":\"a\nb")),
            json!({ "path": "C:\\Users\\me", "body": "a\nb" })
        );
        assert_eq!(
            parse_streaming_json(Some("{\"ok\":1,\"path\":\"C:\\Users")),
            json!({ "ok": 1, "path": "C:\\Users" })
        );
    }

    #[test]
    fn streaming_never_panics_on_garbage() {
        for input in [
            "not json at all",
            "}{",
            "\u{0}\u{1}\u{2}",
            "\"",
            "-",
            "[[[[[[[[",
            "{\"a\":\"\\",
        ] {
            let _ = parse_streaming_json(Some(input));
        }
    }

    #[test]
    fn streaming_object_narrows_non_objects_to_empty() {
        assert_eq!(parse_streaming_json_object(Some("[1,2")), Map::new());
        assert_eq!(parse_streaming_json_object(Some("42")), Map::new());
        assert_eq!(
            parse_streaming_json_object(Some(r#"{"a":1"#)),
            json!({ "a": 1 }).as_object().unwrap().clone()
        );
    }

    /// The behaviour adapters actually depend on: feeding the accumulated text
    /// one character at a time must never panic and must converge on the final
    /// object, growing monotonically in the keys it exposes.
    #[test]
    fn streaming_converges_when_fed_one_character_at_a_time() {
        let full = r#"{"command":"grep -rn \"needle\" .","timeout_ms":5000,"paths":["a/b","c/d"]}"#;
        let chars: Vec<char> = full.chars().collect();
        let mut max_keys = 0usize;
        for end in 0..=chars.len() {
            let prefix: String = chars[..end].iter().collect();
            let parsed = parse_streaming_json_object(Some(&prefix));
            max_keys = max_keys.max(parsed.len());
        }
        assert_eq!(
            parse_streaming_json_object(Some(full)),
            serde_json::from_str::<Map<String, Value>>(full).unwrap()
        );
        assert_eq!(max_keys, 3);
    }

    #[test]
    fn streaming_handles_multibyte_and_emoji_prefixes() {
        let full = r#"{"text":"héllo 🙈 wörld"}"#;
        let chars: Vec<char> = full.chars().collect();
        for end in 0..=chars.len() {
            let prefix: String = chars[..end].iter().collect();
            let _ = parse_streaming_json_object(Some(&prefix));
        }
        assert_eq!(
            parse_streaming_json(Some(full)),
            json!({ "text": "héllo 🙈 wörld" })
        );
    }
}
