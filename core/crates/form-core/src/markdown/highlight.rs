//! Syntax highlighting as **scope names with ranges**, never colors.
//!
//! The core resolves a fence's language to a `syntect` grammar and emits the most specific
//! scope covering each run of code. `FormDesign`'s `SyntaxTokens` (spec 08 §2.5) maps a
//! scope onto the active theme by longest-prefix match. That split is what lets one parser
//! serve three platforms while every colour decision stays in the design system.
//!
//! Ranges are in **UTF-16 code units** measured from the start of the block's `code`
//! string, because that is the unit `NSAttributedString` / `AttributedString` index in;
//! emitting byte offsets would force Swift to re-encode every block on every frame.
//!
//! Runs of pure whitespace and runs whose only scope is the grammar's own root scope are
//! dropped — they carry no colour, and the transcript pays for every token it ships.

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use syntect::easy::ScopeRangeIterator;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use super::CodeToken;

/// A runaway paste must not stall the stream: past either cap the block ships one `plain`
/// token and renders as unhighlighted monospace.
const MAX_BYTES: usize = 200 * 1024;
const MAX_LINES: usize = 5000;

/// Enough to cover every code block on screen plus the scrollback a reader flicks through.
const CACHE_CAPACITY: usize = 512;

const PLAIN: &str = "plain";

/// Scope tokens for one code block, memoized on `(language, code)`.
pub(crate) fn tokens(language: Option<&str>, code: &str) -> Vec<CodeToken> {
    if code.is_empty() {
        return Vec::new();
    }
    let key = cache_key(language, code);
    if let Some(hit) = cache().lock().ok().and_then(|mut c| c.get(key)) {
        return hit;
    }
    let computed = compute(language, code);
    if let Ok(mut c) = cache().lock() {
        c.put(key, computed.clone());
    }
    computed
}

fn compute(language: Option<&str>, code: &str) -> Vec<CodeToken> {
    if code.len() > MAX_BYTES || code.bytes().filter(|b| *b == b'\n').count() >= MAX_LINES {
        return plain(code);
    }
    let Some(language) = language else {
        return plain(code);
    };
    let language = language.trim().to_ascii_lowercase();
    if matches!(language.as_str(), "diff" | "patch" | "udiff") {
        return diff(code);
    }
    let set = syntaxes();
    let Some(syntax) = resolve(set, &language) else {
        return plain(code);
    };
    scoped(set, syntax, code).unwrap_or_else(|| plain(code))
}

/// One token spanning the whole block — unknown language, no language, or over the caps.
fn plain(code: &str) -> Vec<CodeToken> {
    vec![CodeToken {
        start: 0,
        len: utf16_len(code),
        scope: PLAIN.to_string(),
    }]
}

fn scoped(set: &SyntaxSet, syntax: &SyntaxReference, code: &str) -> Option<Vec<CodeToken>> {
    let root = syntax.scope;
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out: Vec<CodeToken> = Vec::new();
    let mut previous: Option<Scope> = None;
    let mut cursor = 0u32;

    for line in LinesWithEndings::from(code) {
        // A grammar can fail on pathological input; the block falls back to plain rather
        // than losing its text.
        let ops = state.parse_line(line, set).ok()?;
        for (range, op) in ScopeRangeIterator::new(&ops, line) {
            stack.apply(op).ok()?;
            if range.is_empty() {
                continue;
            }
            let text = &line[range];
            let width = utf16_len(text);
            let trimmed = text.trim();
            let scope = stack.as_slice().last().copied();
            if let Some(scope) = scope.filter(|s| !trimmed.is_empty() && *s != root) {
                let lead = text.len() - text.trim_start().len();
                let start = cursor + utf16_len(&text[..lead]);
                let len = utf16_len(trimmed);
                let merged = match (previous, out.last_mut()) {
                    (Some(prev), Some(last)) if prev == scope && last.start + last.len == start => {
                        last.len += len;
                        true
                    }
                    _ => false,
                };
                if !merged {
                    out.push(CodeToken {
                        start,
                        len,
                        scope: scope.build_string(),
                    });
                    previous = Some(scope);
                }
            }
            cursor += width;
        }
    }
    Some(out)
}

/// `diff` fences are line-oriented, and the two scopes a reader actually needs are the two
/// the spec names. Anything else (`@@` hunks, headers) stays unstyled.
fn diff(code: &str) -> Vec<CodeToken> {
    let mut out = Vec::new();
    let mut cursor = 0u32;
    for line in LinesWithEndings::from(code) {
        let text = line.trim_end_matches(['\n', '\r']);
        let scope = match text.as_bytes().first() {
            Some(b'+') => Some("markup.inserted"),
            Some(b'-') => Some("markup.deleted"),
            _ => None,
        };
        if let Some(scope) = scope {
            out.push(CodeToken {
                start: cursor,
                len: utf16_len(text),
                scope: scope.to_string(),
            });
        }
        cursor += utf16_len(line);
    }
    out
}

fn utf16_len(s: &str) -> u32 {
    s.chars().map(char::len_utf16).sum::<usize>() as u32
}

// ---------------------------------------------------------------------------------------
// Language resolution
// ---------------------------------------------------------------------------------------

/// `two-face` carries bat's curated syntax dump rather than syntect's defaults, which have
/// no Swift, TypeScript, TOML, Kotlin, Dockerfile, Nix or Zig — languages a coding-agent
/// client cannot afford to render as flat monospace.
///
/// `_newlines` because `ParseState` is fed lines with their terminators; the `_nonewlines`
/// set silently mis-scopes multi-line constructs. Loading the dump is a one-time cost, so
/// it stays behind a `OnceLock` and the app pays it before the first token arrives.
pub(crate) fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn resolve<'a>(set: &'a SyntaxSet, language: &str) -> Option<&'a SyntaxReference> {
    for candidate in aliases(language) {
        if let Some(syntax) = set.find_syntax_by_token(candidate) {
            return Some(syntax);
        }
    }
    set.find_syntax_by_token(language)
}

/// Fence info strings are whatever the model felt like typing. Candidates are tried in
/// order, so an alias can name a preferred grammar and then a graceful substitute — the
/// default syntax set has no TypeScript, and JavaScript is much better than nothing.
fn aliases(language: &str) -> &'static [&'static str] {
    match language {
        "ts" | "typescript" | "mts" | "cts" => &["TypeScript", "js"],
        "tsx" => &["TSX", "TypeScript", "js"],
        "js" | "javascript" | "mjs" | "cjs" | "node" => &["js"],
        "jsx" => &["jsx", "js"],
        "sh" | "zsh" | "bash" | "shell" | "console" | "shell-session" | "ksh" => &["sh", "bash"],
        "yml" | "yaml" => &["yaml"],
        "objc" | "objective-c" | "obj-c" => &["m"],
        "objcpp" | "objective-c++" => &["mm"],
        "c++" | "cpp" | "cxx" | "cc" | "hpp" | "hxx" => &["cpp"],
        "c#" | "csharp" => &["cs"],
        "rust" => &["rs"],
        "python" | "python3" | "py3" => &["py"],
        "ruby" => &["rb"],
        "golang" => &["go"],
        "kotlin" => &["kt"],
        "markdown" => &["md"],
        "jsonc" | "json5" => &["json"],
        "htm" => &["html"],
        "dockerfile" | "docker" => &["Dockerfile"],
        "make" | "mk" => &["Makefile", "make"],
        "text" | "plain" | "plaintext" | "txt" | "log" | "output" => &[],
        _ => &[],
    }
}

// ---------------------------------------------------------------------------------------
// Memoization
// ---------------------------------------------------------------------------------------

/// Re-parsing a growing document re-visits every code block on every tick; only the tail
/// has actually changed, so everything else is a hash lookup (spec 05 §5).
struct Lru {
    map: HashMap<u64, Vec<CodeToken>>,
    order: VecDeque<u64>,
}

impl Lru {
    fn get(&mut self, key: u64) -> Option<Vec<CodeToken>> {
        let hit = self.map.get(&key)?.clone();
        self.touch(key);
        Some(hit)
    }

    fn put(&mut self, key: u64, value: Vec<CodeToken>) {
        if self.map.insert(key, value).is_none() {
            self.order.push_back(key);
        } else {
            self.touch(key);
        }
        while self.order.len() > CACHE_CAPACITY {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            }
        }
    }

    fn touch(&mut self, key: u64) {
        if let Some(pos) = self.order.iter().position(|k| *k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }
}

fn cache() -> &'static Mutex<Lru> {
    static CACHE: OnceLock<Mutex<Lru>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(Lru {
            map: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

fn cache_key(language: Option<&str>, code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    language.hash(&mut hasher);
    code.hash(&mut hasher);
    hasher.finish()
}
