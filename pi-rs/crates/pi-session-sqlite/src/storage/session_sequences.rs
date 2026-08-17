//! `session_sequences` access. Port of `sqlite/storage/session-sequences.ts`.
//!
//! One monotonic counter per session, shared by entries, records, lane moves
//! and facts. Every write reads it, uses it, then advances it inside the same
//! transaction, which is what keeps sequences gap-free.

use rusqlite::Connection;

use pi_session::{SessionError, SessionResult};

use crate::sql::SqlQuery;

pub fn create_sequence(conn: &Connection, session_id: &str, next_seq: i64) -> SessionResult<()> {
    let mut query = SqlQuery::raw("INSERT INTO session_sequences (session_id, next_seq) VALUES (");
    query.bind(session_id).push(", ").bind(next_seq).push(")");
    query.run(conn)?;
    Ok(())
}

pub fn get_next_sequence(conn: &Connection, session_id: &str) -> SessionResult<i64> {
    let mut query = SqlQuery::raw("SELECT next_seq FROM session_sequences WHERE session_id = ");
    query.bind(session_id);
    query.get(conn, |row| row.get(0))?.ok_or_else(|| {
        SessionError::storage(format!("Missing sequence row for session {session_id}"))
    })
}

pub fn set_next_sequence(conn: &Connection, session_id: &str, next_seq: i64) -> SessionResult<()> {
    let mut query = SqlQuery::raw("UPDATE session_sequences SET next_seq = ");
    query
        .bind(next_seq)
        .push(" WHERE session_id = ")
        .bind(session_id);
    query.run(conn)?;
    Ok(())
}

pub fn advance_sequence(conn: &Connection, session_id: &str, seq: i64) -> SessionResult<()> {
    set_next_sequence(conn, session_id, seq + 1)
}

pub fn delete_sequence(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM session_sequences WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}
