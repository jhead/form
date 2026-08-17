//! The SQL layer. Everything here is `GROUP BY` except one deliberate raw pull for the
//! percentile work, which cannot be expressed in SQLite without window functions over a
//! sorted materialisation that costs more than reading the integers.
//!
//! Read-only: the stats engine never writes to the store (spec 01 §1).

use std::collections::HashMap;

use rusqlite::{Connection, Row};

use super::tz::Offsets;
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

// ------------------------------------------------------------------ per-day

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

pub(crate) fn daily(conn: &Connection, w: &Window) -> Result<Vec<DayRow>> {
    let sql = format!(
        "SELECT {day} AS d, COUNT(DISTINCT session_id), COUNT(*), \
                SUM(input), SUM(output), SUM(cache_read), SUM(cache_write), \
                COALESCE(SUM(reasoning), 0), SUM(total_tokens), SUM(cost_total), \
                SUM(duration_ms) \
         FROM turns WHERE started_at >= ?1 AND started_at < ?2 GROUP BY d ORDER BY d",
        day = w.offsets.sql_day("started_at"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([w.from_ms, w.to_ms], |r| {
        Ok(DayRow {
            day: r.get(0)?,
            sessions: u64_at(r, 1)?,
            turns: u64_at(r, 2)?,
            input: u64_at(r, 3)?,
            output: u64_at(r, 4)?,
            cache_read: u64_at(r, 5)?,
            cache_write: u64_at(r, 6)?,
            reasoning: u64_at(r, 7)?,
            total_tokens: u64_at(r, 8)?,
            cost: r.get(9)?,
            duration_ms: u64_at(r, 10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Messages per local day, from the transcript log. Counted separately from turns because
/// a turn is an assistant reply and the dashboard's "messages" line includes the user's.
pub(crate) fn messages_by_day(conn: &Connection, w: &Window) -> Result<HashMap<i64, u64>> {
    let sql = format!(
        "SELECT {day} AS d, COUNT(*) FROM entries \
         WHERE kind = 'message' AND timestamp >= ?1 AND timestamp < ?2 GROUP BY d",
        day = w.offsets.sql_day("timestamp"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([w.from_ms, w.to_ms], |r| Ok((r.get(0)?, u64_at(r, 1)?)))?;
    Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

// ------------------------------------------------------------ weekday × hour

pub(crate) struct WeekHourRow {
    pub(crate) weekday: usize,
    pub(crate) hour: usize,
    pub(crate) total_tokens: u64,
    pub(crate) turns: u64,
}

pub(crate) fn weekday_hour(conn: &Connection, w: &Window) -> Result<Vec<WeekHourRow>> {
    // Day 0 was a Thursday, so `+3` lands Monday on index 0.
    let sql = format!(
        "SELECT (({day} + 3) % 7) AS wd, {hour} AS h, SUM(total_tokens), COUNT(*) \
         FROM turns WHERE started_at >= ?1 AND started_at < ?2 GROUP BY wd, h",
        day = w.offsets.sql_day("started_at"),
        hour = w.offsets.sql_hour("started_at"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([w.from_ms, w.to_ms], |r| {
        Ok(WeekHourRow {
            weekday: r.get::<_, i64>(0)?.clamp(0, 6) as usize,
            hour: r.get::<_, i64>(1)?.clamp(0, 23) as usize,
            total_tokens: u64_at(r, 2)?,
            turns: u64_at(r, 3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ------------------------------------------------------------------- models

pub(crate) struct ModelRow {
    pub(crate) provider_id: String,
    pub(crate) model_id: String,
    pub(crate) turns: u64,
    pub(crate) errors: u64,
    pub(crate) total_tokens: u64,
    pub(crate) cache_read: u64,
    pub(crate) cost: f64,
}

pub(crate) fn models(conn: &Connection, w: &Window) -> Result<Vec<ModelRow>> {
    let mut stmt = conn.prepare(
        "SELECT provider_id, model_id, COUNT(*), \
                SUM(CASE WHEN outcome = 'completed' THEN 0 ELSE 1 END), \
                SUM(total_tokens), SUM(cache_read), SUM(cost_total) \
         FROM turns WHERE started_at >= ?1 AND started_at < ?2 \
         GROUP BY provider_id, model_id ORDER BY SUM(total_tokens) DESC",
    )?;
    let rows = stmt.query_map([w.from_ms, w.to_ms], |r| {
        Ok(ModelRow {
            provider_id: r.get(0)?,
            model_id: r.get(1)?,
            turns: u64_at(r, 2)?,
            errors: u64_at(r, 3)?,
            total_tokens: u64_at(r, 4)?,
            cache_read: u64_at(r, 5)?,
            cost: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One turn's latency inputs. The only raw pull in the engine (spec 03 §4).
pub(crate) struct TurnSample {
    pub(crate) ttft_ms: Option<i64>,
    pub(crate) duration_ms: i64,
    pub(crate) output: u64,
}

/// Samples bucketed by the model's index in [`models`]'s result. The provider/model
/// strings are read as borrowed `&str` and resolved through a nested map, which keeps the
/// hot loop free of allocation — the difference between comfortably inside the 150 ms
/// budget and not.
pub(crate) fn latency_samples(
    conn: &Connection,
    w: &Window,
    models: &[ModelRow],
) -> Result<Vec<Vec<TurnSample>>> {
    let mut index: HashMap<&str, HashMap<&str, usize>> = HashMap::new();
    for (i, m) in models.iter().enumerate() {
        index
            .entry(m.provider_id.as_str())
            .or_default()
            .insert(m.model_id.as_str(), i);
    }

    let mut buckets: Vec<Vec<TurnSample>> = models.iter().map(|_| Vec::new()).collect();
    let mut stmt = conn.prepare(
        "SELECT provider_id, model_id, ttft_ms, duration_ms, output \
         FROM turns WHERE started_at >= ?1 AND started_at < ?2",
    )?;
    let mut rows = stmt.query([w.from_ms, w.to_ms])?;
    while let Some(row) = rows.next()? {
        let provider = row.get_ref(0)?.as_str().map_err(rusqlite::Error::from)?;
        let model = row.get_ref(1)?.as_str().map_err(rusqlite::Error::from)?;
        let Some(&i) = index.get(provider).and_then(|m| m.get(model)) else {
            continue;
        };
        buckets[i].push(TurnSample {
            ttft_ms: row.get(2)?,
            duration_ms: row.get(3)?,
            output: u64_at(row, 4)?,
        });
    }
    Ok(buckets)
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

// ----------------------------------------------------------------- sessions

pub(crate) struct SessionRow {
    pub(crate) session_id: String,
    pub(crate) title: String,
    pub(crate) tokens: u64,
    pub(crate) duration_ms: u64,
    pub(crate) turns: u64,
}

pub(crate) fn sessions(conn: &Connection, w: &Window) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.session_id, COALESCE(s.title, ''), SUM(t.total_tokens), \
                SUM(t.duration_ms), COUNT(*) \
         FROM turns t LEFT JOIN sessions s ON s.id = t.session_id \
         WHERE t.started_at >= ?1 AND t.started_at < ?2 GROUP BY t.session_id",
    )?;
    let rows = stmt.query_map([w.from_ms, w.to_ms], |r| {
        Ok(SessionRow {
            session_id: r.get(0)?,
            title: r.get(1)?,
            tokens: u64_at(r, 2)?,
            duration_ms: u64_at(r, 3)?,
            turns: u64_at(r, 4)?,
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
