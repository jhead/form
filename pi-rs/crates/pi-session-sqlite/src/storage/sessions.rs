//! `sessions` table access. Port of `sqlite/storage/sessions.ts`.

use rusqlite::Connection;
use serde_json::{Map, Value};

use pi_session::state::assert_json_serializable;
use pi_session::{SessionError, SessionMetadata, SessionResult};

use crate::sql::SqlQuery;
use crate::storage::facts::decode_fact_string;

/// The `sessions` row joined with the session's latest `name` fact.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub created_at: i64,
    pub metadata: Option<String>,
    pub cwd: String,
    pub parent_session_id: Option<String>,
    pub has_session_name: bool,
    pub session_name: Option<String>,
}

impl SessionRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            created_at: row.get("created_at")?,
            metadata: row.get("metadata")?,
            cwd: row.get("cwd")?,
            parent_session_id: row.get("parent_session_id")?,
            has_session_name: row.get::<_, i64>("has_session_name")? != 0,
            session_name: row.get("session_name")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewSessionRow {
    pub id: String,
    pub created_at: i64,
    pub cwd: String,
    pub parent_session_id: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}

/// The `SELECT` upstream reuses for both single and list reads: the session row
/// plus its newest global `name` fact.
const SESSION_SELECT: &str = "SELECT s.id, s.created_at, s.metadata, s.cwd, s.parent_session_id,
\t\t\tname_fact.seq IS NOT NULL AS has_session_name,
\t\t\tname_fact.value AS session_name
\t\tFROM sessions AS s
\t\tLEFT JOIN facts AS name_fact
\t\t\tON name_fact.session_id = s.id
\t\t\tAND name_fact.kind = 'name'
\t\t\tAND name_fact.key IS NULL
\t\t\tAND name_fact.seq = (
\t\t\t\tSELECT MAX(f.seq)
\t\t\t\tFROM facts AS f
\t\t\t\tWHERE f.session_id = s.id AND f.kind = 'name' AND f.key IS NULL
\t\t\t)
\t\t";

pub fn session_select() -> &'static str {
    SESSION_SELECT
}

fn parse_metadata(
    metadata: Option<&str>,
    session_id: &str,
) -> SessionResult<Option<Map<String, Value>>> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(metadata).map_err(|_| {
        SessionError::storage(format!(
            "Invalid SQLite session {session_id}: metadata is not valid JSON"
        ))
    })?;
    match parsed {
        Value::Object(map) => Ok(Some(map)),
        _ => Err(SessionError::storage(format!(
            "Invalid SQLite session {session_id}: metadata must be an object"
        ))),
    }
}

pub fn session_exists(conn: &Connection, session_id: &str) -> SessionResult<bool> {
    let mut query = SqlQuery::raw("SELECT 1 AS found FROM sessions WHERE id = ");
    query.bind(session_id);
    query.exists(conn)
}

fn serialize_metadata(metadata: Option<&Map<String, Value>>) -> SessionResult<Option<String>> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    assert_json_serializable(&Value::Object(metadata.clone()))?;
    serde_json::to_string(metadata).map(Some).map_err(|error| {
        SessionError::invalid_payload(format!(
            "SQLite session metadata is not serializable: {error}"
        ))
    })
}

pub fn insert_session_row(conn: &Connection, session: &NewSessionRow) -> SessionResult<()> {
    let metadata = serialize_metadata(session.metadata.as_ref())?;
    let mut query = SqlQuery::raw(
        "INSERT INTO sessions (id, created_at, metadata, cwd, parent_session_id)\n\t\tVALUES (",
    );
    query
        .bind(session.id.as_str())
        .push(", ")
        .bind(session.created_at)
        .push(", ")
        .bind(metadata)
        .push(", ")
        .bind(session.cwd.as_str())
        .push(", ")
        .bind(session.parent_session_id.clone())
        .push(")");
    query.run(conn)?;
    Ok(())
}

pub fn read_session_row(conn: &Connection, session_id: &str) -> SessionResult<Option<SessionRow>> {
    let mut query = SqlQuery::raw(SESSION_SELECT);
    query.push("WHERE s.id = ").bind(session_id);
    query.get(conn, SessionRow::from_row)
}

pub fn read_session_rows(conn: &Connection, cwd: Option<&str>) -> SessionResult<Vec<SessionRow>> {
    let mut query = SqlQuery::raw(SESSION_SELECT);
    if let Some(cwd) = cwd {
        query.push("WHERE s.cwd = ").bind(cwd);
    }
    query.push("\n\t\tORDER BY s.created_at DESC");
    query.all(conn, SessionRow::from_row)
}

pub fn delete_session_row(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM sessions WHERE id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}

/// Projects a row into the port's flat [`SessionMetadata`].
///
/// Upstream's `SqliteSessionMetadata` widens the base type with `cwd`, `path`,
/// `name` and `metadata`; the port has no metadata type parameter, so those
/// ride in [`SessionMetadata::extra`]. A cleared name is *absent*, not null —
/// the upstream tests assert `not.toHaveProperty("name")`.
pub fn decode_session_metadata(row: &SessionRow, path: &str) -> SessionResult<SessionMetadata> {
    let metadata = parse_metadata(row.metadata.as_deref(), &row.id)?;
    let name = if row.has_session_name {
        decode_fact_string(row.session_name.as_deref(), &row.id, "name")?
    } else {
        None
    };
    let mut result = SessionMetadata::new(row.id.clone(), row.created_at);
    if let Some(name) = name {
        result.set("name", Value::String(name));
    }
    result.set("cwd", Value::String(row.cwd.clone()));
    result.set("path", Value::String(path.to_string()));
    result.parent_session_id = row.parent_session_id.clone();
    if let Some(metadata) = metadata {
        result.set("metadata", Value::Object(metadata));
    }
    Ok(result)
}
