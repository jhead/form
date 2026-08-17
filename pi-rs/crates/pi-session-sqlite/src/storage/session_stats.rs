//! `session_stats` access. Port of `sqlite/storage/session-stats.ts`.
//!
//! The token columns are `REAL` upstream (JavaScript has one number type), so
//! they stay `REAL` here for wire compatibility and are narrowed to `i64` on
//! read. Corrective usage records carry negative deltas, which is why the
//! counters are signed.

use rusqlite::Connection;

use pi_core::Usage;
use pi_session::{SessionError, SessionResult, SessionStats};

use crate::sql::SqlQuery;

pub fn create_stats(conn: &Connection, session_id: &str, message_count: i64) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "INSERT INTO session_stats\n\t\t\t(session_id, message_count, cached_tokens, uncached_tokens, total_tokens, cost_total)\n\t\t\tVALUES (",
    );
    query
        .bind(session_id)
        .push(", ")
        .bind(message_count)
        .push(", 0, 0, 0, 0)");
    query.run(conn)?;
    Ok(())
}

pub fn read_stats(conn: &Connection, session_id: &str) -> SessionResult<SessionStats> {
    let mut query = SqlQuery::raw(
        "SELECT session_id, message_count, cached_tokens, uncached_tokens, total_tokens, cost_total\n\t\tFROM session_stats\n\t\tWHERE session_id = ",
    );
    query.bind(session_id);
    query
        .get(conn, |row| {
            Ok(SessionStats {
                message_count: row.get::<_, i64>("message_count")?,
                cached_tokens: row.get::<_, f64>("cached_tokens")? as i64,
                uncached_tokens: row.get::<_, f64>("uncached_tokens")? as i64,
                total_tokens: row.get::<_, f64>("total_tokens")? as i64,
                cost_total: row.get::<_, f64>("cost_total")?,
            })
        })?
        .ok_or_else(|| SessionError::storage(format!("Missing stats row for session {session_id}")))
}

pub fn increment_message_count(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "UPDATE session_stats SET message_count = message_count + 1 WHERE session_id = ",
    );
    query.bind(session_id);
    if query.run(conn)? != 1 {
        return Err(SessionError::storage(format!(
            "Missing stats row for session {session_id}"
        )));
    }
    Ok(())
}

pub fn add_usage_to_stats(conn: &Connection, session_id: &str, usage: &Usage) -> SessionResult<()> {
    let mut query = SqlQuery::raw("UPDATE session_stats\n\t\tSET cached_tokens = cached_tokens + ");
    query
        .bind(usage.cache_read)
        .push(",\n\t\t\tuncached_tokens = uncached_tokens + ")
        .bind(usage.input + usage.cache_write)
        .push(",\n\t\t\ttotal_tokens = total_tokens + ")
        .bind(usage.total_tokens)
        .push(",\n\t\t\tcost_total = cost_total + ")
        .bind(usage.cost.total)
        .push("\n\t\tWHERE session_id = ")
        .bind(session_id);
    if query.run(conn)? != 1 {
        return Err(SessionError::storage(format!(
            "Missing stats row for session {session_id}"
        )));
    }
    Ok(())
}

pub fn delete_stats(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM session_stats WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}
