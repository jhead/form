//! Full-text search over session titles and message bodies (F13.3).
//!
//! FTS5's `snippet()` returns a marked-up string. Swift must never parse markup out of a
//! string, so we ask for markers no user would type, strip them, and hand back explicit
//! `{start, len}` ranges in **UTF-16 code units** — the unit `NSAttributedString` and
//! `AttributedString` index in.

use rusqlite::params;

use crate::error::Result;
use crate::protocol::{HighlightRange, SearchHit};

use super::store::Store;

/// Private-use markers: `snippet()` wraps matches in these, and they cannot collide with
/// anything in a transcript.
const OPEN: char = '\u{e000}';
const CLOSE: char = '\u{e001}';

const DEFAULT_LIMIT: usize = 30;
/// Title matches should outrank a passing mention in a long transcript.
const TITLE_WEIGHT: f64 = 8.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    /// Every non-archived session — the `⌘K` palette (F13.1).
    All,
    /// One session's transcript — the `⌘F` find bar (F13.2).
    Session(String),
}

impl Store {
    pub fn search(&self, q: &str, scope: SearchScope, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(match_expr) = fts_query(q) else {
            return Ok(Vec::new());
        };
        let limit = if limit == 0 { DEFAULT_LIMIT } else { limit };
        let session_filter = match &scope {
            SearchScope::All => None,
            SearchScope::Session(id) => Some(id.clone()),
        };

        // Archived sessions are excluded from the global palette, so over-fetch and filter
        // rather than paying for a join against an fts5 table.
        let fetch = (limit * 4 + 20) as i64;

        // The snippet markers and bm25 weights are compile-time constants, not user input;
        // fts5 auxiliary functions want them inline.
        let sql = format!(
            "SELECT session_id, entry_id,
                    snippet(search, 0, '{OPEN}', '{CLOSE}', '…', 14),
                    snippet(search, 1, '{OPEN}', '{CLOSE}', '…', 14),
                    bm25(search, {TITLE_WEIGHT}, 1.0, 0.0, 0.0)
             FROM search
             WHERE search MATCH ?1 AND (?2 IS NULL OR session_id = ?2)
             ORDER BY bm25(search, {TITLE_WEIGHT}, 1.0, 0.0, 0.0) ASC
             LIMIT ?3"
        );

        let rows = self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![match_expr, session_filter, fetch], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, f64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;

        let mut hits = Vec::with_capacity(limit.min(rows.len()));
        for (session_id, entry_id, title_snip, body_snip, score) in rows {
            let is_title_row = entry_id.is_empty();
            let raw = if is_title_row { title_snip } else { body_snip };
            let (snippet, highlights) = split_markers(&raw);
            if highlights.is_empty() {
                // fts5 matched a column we are not displaying (the title text stored on the
                // title row can match while the body snippet is empty); nothing to show.
                continue;
            }

            let Some((title, archived, timestamp)) = self.hit_context(
                &session_id,
                if is_title_row { None } else { Some(&entry_id) },
            )?
            else {
                continue;
            };
            if archived && matches!(scope, SearchScope::All) {
                continue;
            }

            hits.push(SearchHit {
                session_id,
                entry_id: (!is_title_row).then_some(entry_id),
                title,
                snippet,
                highlights,
                // bm25 is "more negative is better"; flip it so a bigger score is a better
                // hit, which is what every caller expects.
                score: -score,
                timestamp,
            });
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
    }

    /// Title, archived flag and timestamp for a hit — the fts5 table stores none of them.
    fn hit_context(
        &self,
        session_id: &str,
        entry_id: Option<&str>,
    ) -> Result<Option<(String, bool, i64)>> {
        self.with_conn(|conn| {
            let session: Option<(String, bool, i64)> = conn
                .query_row(
                    "SELECT title, archived, updated_at FROM sessions WHERE id = ?1",
                    params![session_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let Some((title, archived, updated_at)) = session else {
                return Ok(None);
            };
            let timestamp = match entry_id {
                Some(id) => conn
                    .query_row(
                        "SELECT timestamp FROM entries WHERE id = ?1",
                        params![id],
                        |r| r.get(0),
                    )
                    .unwrap_or(updated_at),
                None => updated_at,
            };
            Ok(Some((title, archived, timestamp)))
        })
    }
}

/// Turn free text into an FTS5 MATCH expression.
///
/// User input is never passed through raw: `AND`, `NEAR`, `*`, `"` and `:` are all operators
/// in fts5's grammar, so a query like `foo:` is a syntax error rather than a search. Every
/// run of alphanumerics becomes a quoted term, ANDed together, with the last one made a
/// prefix so search feels live as you type.
fn fts_query(q: &str) -> Option<String> {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    if terms.is_empty() {
        return None;
    }
    let last = terms.len() - 1;
    Some(
        terms
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == last && t.len() >= 2 {
                    format!("\"{t}\"*")
                } else {
                    format!("\"{t}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Strip the marker pair, recording each highlight as a UTF-16 `{start, len}`.
fn split_markers(raw: &str) -> (String, Vec<HighlightRange>) {
    let mut text = String::with_capacity(raw.len());
    let mut ranges = Vec::new();
    let mut utf16 = 0u32;
    let mut open_at: Option<u32> = None;

    for ch in raw.chars() {
        match ch {
            OPEN => open_at = Some(utf16),
            CLOSE => {
                if let Some(start) = open_at.take() {
                    if utf16 > start {
                        ranges.push(HighlightRange {
                            start,
                            len: utf16 - start,
                        });
                    }
                }
            }
            _ => {
                text.push(ch);
                utf16 += ch.len_utf16() as u32;
            }
        }
    }
    (text, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_safe_match_expression() {
        assert_eq!(
            fts_query("health check"),
            Some("\"health\" \"check\"*".into())
        );
        // Operators and punctuation are neutralized rather than passed through.
        assert_eq!(fts_query("foo: AND"), Some("\"foo\" \"and\"*".into()));
        assert_eq!(fts_query("  \"  "), None);
        assert_eq!(fts_query(""), None);
        // A one-character tail is not made a prefix — `"a"*` matches nearly everything.
        assert_eq!(fts_query("rust a"), Some("\"rust\" \"a\"".into()));
    }

    #[test]
    fn marker_ranges_are_utf16_and_markers_are_stripped() {
        let raw = format!("café {OPEN}search{CLOSE} here");
        let (text, ranges) = split_markers(&raw);
        assert_eq!(text, "café search here");
        assert_eq!(ranges, vec![HighlightRange { start: 5, len: 6 }]);

        // An emoji is two UTF-16 code units, one Rust char.
        let raw = format!("🚀 {OPEN}go{CLOSE}");
        let (text, ranges) = split_markers(&raw);
        assert_eq!(text, "🚀 go");
        assert_eq!(ranges, vec![HighlightRange { start: 3, len: 2 }]);
    }

    #[test]
    fn unmatched_markers_do_not_panic() {
        let (text, ranges) = split_markers(&format!("dangling {OPEN}open"));
        assert_eq!(text, "dangling open");
        assert!(ranges.is_empty());
    }
}
