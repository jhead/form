//! The SQL layer. Everything here is `GROUP BY` except one deliberate raw pull for the
//! percentile work, which cannot be expressed in SQLite without window functions over a
//! sorted materialisation that costs more than reading the integers.
//!
//! Read-only: the stats engine never writes to the store (spec 01 §1).

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, Row};

use super::tz::{Offsets, DAY_MS, HOUR_MS};
use crate::error::Result;

/// The half-open UTC window `[from_ms, to_ms)` and the offsets that bucket it.
pub(crate) struct Window {
    pub(crate) from_ms: i64,
    pub(crate) to_ms: i64,
    pub(crate) offsets: Offsets,
}

pub(crate) fn has_table(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

/// `(min, max)` of `turns.started_at`, or `None` when there are no turns at all.
pub(crate) fn turn_bounds(conn: &Connection) -> Result<Option<(i64, i64)>> {
    let bounds = conn.query_row(
        "SELECT MIN(started_at), MAX(started_at) FROM turns",
        [],
        |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
    )?;
    Ok(match bounds {
        (Some(lo), Some(hi)) => Some((lo, hi)),
        _ => None,
    })
}

fn u64_at(row: &Row, idx: usize) -> rusqlite::Result<u64> {
    Ok(row.get::<_, i64>(idx)?.max(0) as u64)
}

/// Borrowed rather than owned: the scan reads four text columns per row and allocating
/// them would cost more than everything else it does.
fn str_at(row: &Row, idx: usize) -> rusqlite::Result<&str> {
    Ok(row.get_ref(idx)?.as_str()?)
}

// ------------------------------------------------------------- the turn scan

/// Everything `turns` contributes to the document, accumulated in one pass.
///
/// Spec 03 §4 asks for `GROUP BY` and a raw pull only for percentiles, and that is where
/// this started — but five groupings mean five scans of the same 100k rows, and SQLite
/// builds a temporary b-tree for each because no index covers a local-day expression or a
/// provider/model pair. Measured on the 100k-turn fixture: 159 ms across those five
/// statements against 34 ms for this single pass, which is the difference between missing
/// and meeting the budget. The aggregation SQLite still does well — narrow tables, index
/// ranges, `tool_invocations` — stays in SQL.
#[derive(Default)]
pub(crate) struct Scan {
    /// Per local day, ascending.
    pub(crate) days: Vec<DayRow>,
    /// `[weekday][hour]` tokens, Monday = 0.
    pub(crate) weekday_hour: Vec<Vec<u64>>,
    /// `(tokens, turns)` per local hour.
    pub(crate) hourly: Vec<(u64, u64)>,
    /// Descending by tokens, each with its raw latency samples.
    pub(crate) models: Vec<(ModelRow, Vec<TurnSample>)>,
    pub(crate) sessions: Vec<SessionRow>,
}

pub(crate) struct DayRow {
    pub(crate) day: i64,
    pub(crate) sessions: u64,
    pub(crate) turns: u64,
    pub(crate) input: u64,
    pub(crate) output: u64,
    pub(crate) cache_read: u64,
    pub(crate) cache_write: u64,
    pub(crate) reasoning: u64,
    pub(crate) total_tokens: u64,
    pub(crate) cost: f64,
    pub(crate) duration_ms: u64,
}

pub(crate) struct ModelRow {
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    pub(crate) turns: u64,
    pub(crate) errors: u64,
    pub(crate) total_tokens: u64,
    pub(crate) cache_read: u64,
    pub(crate) cost: f64,
}

/// One turn's latency inputs, kept raw because nearest-rank percentiles need the sample,
/// not a moment (spec 03 §3).
pub(crate) struct TurnSample {
    pub(crate) ttft_ms: Option<i64>,
    pub(crate) duration_ms: i64,
    pub(crate) output: u64,
}

pub(crate) struct SessionRow {
    pub(crate) session_id: String,
    pub(crate) tokens: u64,
    pub(crate) duration_ms: u64,
    pub(crate) turns: u64,
}

pub(crate) fn scan_turns(conn: &Connection, w: &Window) -> Result<Scan> {
    let mut days: HashMap<i64, DayRow> = HashMap::new();
    let mut weekday_hour = vec![vec![0u64; 24]; 7];
    let mut hourly = vec![(0u64, 0u64); 24];
    // `(day, session index)` — the distinct-sessions-per-day count SQL would have done
    // with COUNT(DISTINCT), without a second pass.
    let mut day_sessions: HashSet<(i64, u32)> = HashSet::new();

    let mut session_index: HashMap<String, usize> = HashMap::new();
    let mut sessions: Vec<SessionRow> = Vec::new();
    // Nested so the hot loop can look up a borrowed `&str` pair without allocating.
    let mut model_index: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut models: Vec<(ModelRow, Vec<TurnSample>)> = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT started_at, session_id, provider_id, model_id, ttft_ms, duration_ms, \
                input, output, cache_read, cache_write, reasoning, total_tokens, \
                cost_total, outcome \
         FROM turns WHERE started_at >= ?1 AND started_at < ?2",
    )?;
    let mut rows = stmt.query([w.from_ms, w.to_ms])?;
    while let Some(row) = rows.next()? {
        let started: i64 = row.get(0)?;
        let input = u64_at(row, 6)?;
        let output = u64_at(row, 7)?;
        let cache_read = u64_at(row, 8)?;
        let cache_write = u64_at(row, 9)?;
        let reasoning = row.get::<_, Option<i64>>(10)?.unwrap_or(0).max(0) as u64;
        let total_tokens = u64_at(row, 11)?;
        let cost: f64 = row.get(12)?;
        let duration_ms: i64 = row.get(5)?;

        let local = w.offsets.local_ms(started);
        let day = local.div_euclid(DAY_MS);
        let hour = (local.rem_euclid(DAY_MS) / HOUR_MS) as usize;
        // Day 0 (1970-01-01) was a Thursday, so `+3` lands Monday on index 0.
        let weekday = (day + 3).rem_euclid(7) as usize;

        let bucket = days.entry(day).or_insert_with(|| DayRow::empty(day));
        bucket.turns += 1;
        bucket.input += input;
        bucket.output += output;
        bucket.cache_read += cache_read;
        bucket.cache_write += cache_write;
        bucket.reasoning += reasoning;
        bucket.total_tokens += total_tokens;
        bucket.cost += cost;
        bucket.duration_ms += duration_ms.max(0) as u64;

        weekday_hour[weekday][hour] += total_tokens;
        hourly[hour].0 += total_tokens;
        hourly[hour].1 += 1;

        let session_id = str_at(row, 1)?;
        let s = match session_index.get(session_id) {
            Some(&i) => i,
            None => {
                sessions.push(SessionRow {
                    session_id: session_id.to_string(),
                    tokens: 0,
                    duration_ms: 0,
                    turns: 0,
                });
                session_index.insert(session_id.to_string(), sessions.len() - 1);
                sessions.len() - 1
            }
        };
        sessions[s].tokens += total_tokens;
        sessions[s].duration_ms += duration_ms.max(0) as u64;
        sessions[s].turns += 1;
        day_sessions.insert((day, s as u32));

        let provider = str_at(row, 2)?;
        let model_id = str_at(row, 3)?;
        let m = match model_index.get(provider).and_then(|m| m.get(model_id)) {
            Some(&i) => i,
            None => {
                models.push((
                    ModelRow {
                        provider_id: provider.to_string(),
                        model_id: model_id.to_string(),
                        turns: 0,
                        errors: 0,
                        total_tokens: 0,
                        cache_read: 0,
                        cost: 0.0,
                    },
                    Vec::new(),
                ));
                model_index
                    .entry(provider.to_string())
                    .or_default()
                    .insert(model_id.to_string(), models.len() - 1);
                models.len() - 1
            }
        };
        let (stats, samples) = &mut models[m];
        stats.turns += 1;
        stats.total_tokens += total_tokens;
        stats.cache_read += cache_read;
        stats.cost += cost;
        // Failed and aborted turns count, and keep whatever tokens they produced.
        if str_at(row, 13)? != "completed" {
            stats.errors += 1;
        }
        samples.push(TurnSample {
            ttft_ms: row.get(4)?,
            duration_ms,
            output,
        });
    }

    for (day, _session) in day_sessions {
        if let Some(bucket) = days.get_mut(&day) {
            bucket.sessions += 1;
        }
    }
    let mut days: Vec<DayRow> = days.into_values().collect();
    days.sort_unstable_by_key(|d| d.day);
    models.sort_by(|a, b| b.0.total_tokens.cmp(&a.0.total_tokens));

    Ok(Scan {
        days,
        weekday_hour,
        hourly,
        models,
        sessions,
    })
}

impl DayRow {
    fn empty(day: i64) -> Self {
        Self {
            day,
            sessions: 0,
            turns: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
            total_tokens: 0,
            cost: 0.0,
            duration_ms: 0,
        }
    }
}

/// Titles for the ranked sessions only — the leaderboards are 10 rows each, so joining
/// every session in the period to fetch a string nobody renders would be waste.
pub(crate) fn titles(conn: &Connection, ids: &[String]) -> Result<HashMap<String, String>> {
    let mut titles = HashMap::new();
    if ids.is_empty() {
        return Ok(titles);
    }
    let sql = format!(
        "SELECT id, title FROM sessions WHERE id IN ({})",
        vec!["?"; ids.len()].join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(ids))?;
    while let Some(row) = rows.next()? {
        titles.insert(row.get(0)?, row.get(1)?);
    }
    Ok(titles)
}

// -------------------------------------------------------------------- tools

pub(crate) struct ToolRow {
    pub(crate) name: String,
    pub(crate) invocations: u64,
    pub(crate) errors: u64,
    pub(crate) duration_ms: u64,
}

pub(crate) fn tools(conn: &Connection, w: &Window) -> Result<Vec<ToolRow>> {
    let mut stmt = conn.prepare(
        "SELECT tool_name, COUNT(*), SUM(is_error), SUM(duration_ms) \
         FROM tool_invocations WHERE started_at >= ?1 AND started_at < ?2 \
         GROUP BY tool_name ORDER BY COUNT(*) DESC LIMIT 40",
    )?;
    let rows = stmt.query_map([w.from_ms, w.to_ms], |r| {
        Ok(ToolRow {
            name: r.get(0)?,
            invocations: u64_at(r, 1)?,
            errors: u64_at(r, 2)?,
            duration_ms: u64_at(r, 3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ------------------------------------------------------------- whole-history

/// Distinct local days with at least one turn, over all history. Covered by
/// `turns_started`, so this is an index-only scan however long the history is.
pub(crate) fn active_days_all_time(conn: &Connection, offsets: &Offsets) -> Result<Vec<i64>> {
    let sql = format!(
        "SELECT DISTINCT {day} AS d FROM turns WHERE started_at >= 0 ORDER BY d",
        day = offsets.sql_day("started_at"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `(cost, active days)` over the trailing 14 local days, independent of the selected
/// range — a 7-day view still projects from a 14-day basis (spec 03 §3).
pub(crate) fn trailing_cost(
    conn: &Connection,
    offsets: &Offsets,
    from_ms: i64,
) -> Result<(f64, u32)> {
    let sql = format!(
        "SELECT COALESCE(SUM(cost_total), 0.0), COUNT(DISTINCT {day}) \
         FROM turns WHERE started_at >= ?1",
        day = offsets.sql_day("started_at"),
    );
    Ok(conn.query_row(&sql, [from_ms], |r| {
        Ok((r.get(0)?, r.get::<_, i64>(1)?.max(0) as u32))
    })?)
}
