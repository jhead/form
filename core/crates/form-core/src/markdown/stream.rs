//! Repairing the tail of a document that is still being written.
//!
//! During streaming `parse_streaming` is called on every debounce tick against a document
//! that grows by a few tokens at a time. Left alone, CommonMark renders a half-typed
//! construct as its literal source — `**bol`, `[docs](htt`, a bare `| a | b |` row — which
//! means the reader watches punctuation appear and then vanish, and the paragraph reflows
//! every time. F7.3 asks for the opposite: the construct should render as the thing it is
//! about to become.
//!
//! So before parsing an incomplete document we rewrite **only its trailing region** — the
//! text after the last blank line or closed fence, which is the only part that can still
//! change — closing dangling emphasis, dropping half-typed link syntax, and giving a
//! half-written table the delimiter row it needs. Everything before that region is
//! byte-identical to what a complete parse would see, so every earlier block keeps its id.
//!
//! These are heuristics, and deliberately conservative ones: a construct is only repaired
//! when its opener passes CommonMark's flanking rules, so `2 * 3` and `snake_case` are left
//! alone. The cost of a wrong guess is one frame of the wrong style; the cost of not
//! guessing is a paragraph that reflows on every token.

use std::borrow::Cow;

/// Rewrite the trailing incomplete construct of `text`, if any.
pub(crate) fn repair_tail(text: &str) -> Cow<'_, str> {
    let Some(tail_start) = tail_region(text) else {
        // Inside an unterminated fence: the content is verbatim code, and pulldown-cmark
        // already closes the block at EOF. Nothing to repair.
        return Cow::Borrowed(text);
    };
    let tail = &text[tail_start..];
    if tail.trim().is_empty() {
        return Cow::Borrowed(text);
    }

    let inline = repair_inline(tail);
    let current = inline.as_deref().unwrap_or(tail);
    let table = repair_table(current);

    match (inline, table) {
        (_, Some(t)) => Cow::Owned(format!("{}{}", &text[..tail_start], t)),
        (Some(t), None) => Cow::Owned(format!("{}{}", &text[..tail_start], t)),
        (None, None) => Cow::Borrowed(text),
    }
}

/// Byte offset of the trailing region: after the last blank line or closed fence. `None`
/// when the document ends inside a fence.
fn tail_region(text: &str) -> Option<usize> {
    let mut fence: Option<(u8, usize)> = None;
    let mut start = 0usize;
    let mut offset = 0usize;

    for raw in text.split_inclusive('\n') {
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        match fence {
            Some((ch, len)) => {
                if closes_fence(line, ch, len) {
                    fence = None;
                    start = offset;
                }
            }
            None => {
                if let Some(open) = opens_fence(line) {
                    fence = Some(open);
                } else if line.trim().is_empty() {
                    start = offset;
                }
            }
        }
    }

    fence.is_none().then_some(start)
}

fn fence_run(line: &str) -> Option<(u8, usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = match rest.as_bytes().first()? {
        b'`' => b'`',
        b'~' => b'~',
        _ => return None,
    };
    let len = rest.bytes().take_while(|b| *b == ch).count();
    (len >= 3).then(|| (ch, len, &rest[len..]))
}

fn opens_fence(line: &str) -> Option<(u8, usize)> {
    let (ch, len, info) = fence_run(line)?;
    // A backtick fence's info string may not contain a backtick.
    (ch != b'`' || !info.contains('`')).then_some((ch, len))
}

fn closes_fence(line: &str, ch: u8, len: usize) -> bool {
    matches!(fence_run(line), Some((c, n, rest)) if c == ch && n >= len && rest.trim().is_empty())
}

// ---------------------------------------------------------------------------------------
// Inline repair
// ---------------------------------------------------------------------------------------

/// One unclosed delimiter run: byte offset, delimiter byte, run length.
type Delim = (usize, u8, usize);

enum Scan {
    /// A link or image whose destination never arrived. Its visible text survives; the
    /// syntax around it is dropped so nothing reflows when the `)` finally lands.
    Link { start: usize, text: (usize, usize) },
    /// Delimiter runs still open at end of input, outermost first.
    Open(Vec<Delim>),
}

fn repair_inline(tail: &str) -> Option<String> {
    if is_indented_code(tail) {
        return None;
    }

    let mut current = Cow::Borrowed(tail);
    // Each link cut strictly shortens the text, so this terminates; the bound is only
    // there to keep a pathological input from walking a long string repeatedly.
    for _ in 0..8 {
        match scan(&current) {
            Scan::Link { start, text } => {
                let cut = format!("{}{}", &current[..start], &current[text.0..text.1]);
                current = Cow::Owned(cut);
            }
            Scan::Open(open) => {
                return match close_delims(&current, open) {
                    Some(closed) => Some(closed),
                    // No dangling emphasis, but an earlier pass may still have cut a link.
                    None => owned(current),
                };
            }
        }
    }
    owned(current)
}

fn owned(text: Cow<'_, str>) -> Option<String> {
    match text {
        Cow::Owned(s) => Some(s),
        Cow::Borrowed(_) => None,
    }
}

/// A trailing region that is entirely indented four spaces is an indented code block; its
/// content is verbatim and must not be touched.
fn is_indented_code(tail: &str) -> bool {
    tail.lines()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.starts_with("    ") || l.starts_with('\t'))
}

fn scan(s: &str) -> Scan {
    let b = s.as_bytes();
    let mut open: Vec<Delim> = Vec::new();
    // (offset of the construct including a leading `!`, offset of the `[`)
    let mut brackets: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;

    while i < b.len() {
        // Inside a code span nothing is markup but the closing backtick run.
        if let Some(&(_, b'`', run)) = open.last() {
            if b[i] == b'`' {
                let n = run_len(b, i, b'`');
                if n == run {
                    open.pop();
                }
                i += n;
            } else {
                i += 1;
            }
            continue;
        }

        match b[i] {
            b'\\' => i += 2,
            b'`' => {
                let n = run_len(b, i, b'`');
                open.push((i, b'`', n));
                i += n;
            }
            c @ (b'*' | b'_' | b'~') => i += delimiter(s, i, c, &mut open),
            b'[' => {
                let start = if i > 0 && b[i - 1] == b'!' { i - 1 } else { i };
                brackets.push((start, i));
                i += 1;
            }
            b']' => {
                let Some((start, bracket)) = brackets.pop() else {
                    i += 1;
                    continue;
                };
                if b.get(i + 1) == Some(&b'(') {
                    match close_paren(b, i + 1) {
                        Some(end) => i = end + 1,
                        None => {
                            return Scan::Link {
                                start,
                                text: (bracket + 1, i),
                            }
                        }
                    }
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    if let Some(&(start, bracket)) = brackets.first() {
        return Scan::Link {
            start,
            text: (bracket + 1, s.len()),
        };
    }
    Scan::Open(open)
}

fn run_len(b: &[u8], i: usize, c: u8) -> usize {
    b[i..].iter().take_while(|x| **x == c).count()
}

fn close_paren(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Push or pop one emphasis/strikethrough delimiter run, applying CommonMark's flanking
/// rules loosely enough to be useful and tightly enough that `2 * 3` stays arithmetic.
/// Returns the number of bytes consumed.
fn delimiter(s: &str, i: usize, c: u8, open: &mut Vec<Delim>) -> usize {
    let b = s.as_bytes();
    let n = run_len(b, i, c);
    let significant = match c {
        b'~' => n == 2,
        _ => n <= 3,
    };
    if !significant {
        return n;
    }

    let prev = s[..i].chars().next_back();
    let next = s[i + n..].chars().next();

    // `_` never emphasises inside a word, and a `*` opening a line is a list bullet.
    if c == b'_'
        && prev.is_some_and(|p| p.is_alphanumeric())
        && next.is_some_and(|p| p.is_alphanumeric())
    {
        return n;
    }
    if c == b'*' && n == 1 && at_line_start(s, i) && next.is_some_and(char::is_whitespace) {
        return n;
    }

    let can_close = prev.is_some_and(|p| !p.is_whitespace());
    if can_close {
        if let Some(pos) = open.iter().rposition(|&(_, dc, dn)| dc == c && dn == n) {
            open.truncate(pos);
            return n;
        }
    }

    // Opening intra-word (`3*4`) is legal CommonMark but is nearly always arithmetic or a
    // glob in a chat transcript, so only open at a word boundary.
    let can_open = next.is_some_and(|p| !p.is_whitespace())
        && prev.is_none_or(|p| p.is_whitespace() || "([{\"'".contains(p));
    if can_open {
        open.push((i, c, n));
    }
    n
}

fn at_line_start(s: &str, i: usize) -> bool {
    s[..i]
        .chars()
        .rev()
        .take_while(|c| *c != '\n')
        .all(|c| c == ' ' || c == '\t')
}

/// Close what is still open, innermost first — or, when a delimiter has nothing after it
/// yet, drop it rather than emit `****`.
fn close_delims(s: &str, mut open: Vec<Delim>) -> Option<String> {
    let mut cut = s.len();
    while let Some(&(pos, _, n)) = open.last() {
        if s[pos + n..cut].trim().is_empty() {
            cut = pos;
            open.pop();
        } else {
            break;
        }
    }
    if open.is_empty() && cut == s.len() {
        return None;
    }

    // Closers go before the region's trailing whitespace so emphasis does not swallow the
    // soft break at the end of the buffer.
    let head = &s[..cut];
    let end = head.trim_end().len();
    let mut out = String::with_capacity(s.len() + 8);
    out.push_str(&head[..end]);
    for &(_, c, n) in open.iter().rev() {
        for _ in 0..n {
            out.push(c as char);
        }
    }
    out.push_str(&head[end..]);
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Table repair
// ---------------------------------------------------------------------------------------

/// Give a half-written table the delimiter row GFM requires, so a header row renders as a
/// header instead of a paragraph that turns into a table two tokens later.
fn repair_table(tail: &str) -> Option<String> {
    let trailing_newline = tail.ends_with('\n');
    let body = tail.strip_suffix('\n').unwrap_or(tail);
    let mut lines: Vec<&str> = body.split('\n').collect();
    if lines.is_empty() || !lines[0].trim_start().starts_with('|') {
        return None;
    }
    if !lines.iter().all(|l| l.trim_start().starts_with('|')) {
        return None;
    }

    let cols = cell_count(lines[0]);
    if cols == 0 {
        return None;
    }
    if lines.iter().skip(1).any(|l| is_delimiter_row(l, cols)) {
        return None; // already a table as far as the parser is concerned
    }

    let row = delimiter_row(cols);
    if lines.len() > 1 && looks_like_partial_delimiter(lines[lines.len() - 1]) {
        let last = lines.len() - 1;
        lines[last] = row.as_str();
    } else {
        lines.insert(1, row.as_str());
    }

    let mut out = lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    Some(out)
}

fn cells(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').collect()
}

fn cell_count(line: &str) -> usize {
    cells(line).len()
}

fn is_delimiter_row(line: &str, cols: usize) -> bool {
    let parts = cells(line);
    parts.len() == cols
        && parts.iter().all(|c| {
            let c = c.trim();
            let core = c.trim_start_matches(':').trim_end_matches(':');
            !core.is_empty() && core.bytes().all(|b| b == b'-')
        })
}

fn looks_like_partial_delimiter(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.bytes().all(|b| matches!(b, b'-' | b':' | b'|' | b' '))
}

fn delimiter_row(cols: usize) -> String {
    let mut s = String::from("|");
    for _ in 0..cols {
        s.push_str(" --- |");
    }
    s
}
