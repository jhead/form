//! `entries` table access. Port of `sqlite/storage/entries.ts`.

use rusqlite::Connection;
use serde_json::{Map, Value};

use pi_session::{Entry, EntryOrder, EntryType, SessionError, SessionResult};

use crate::sql::SqlQuery;

/// A row of `entries`.
#[derive(Debug, Clone)]
pub struct EntryRow {
    pub session_id: String,
    pub seq: i64,
    pub id: String,
    pub parent_id: Option<String>,
    pub entry_type: String,
    pub timestamp: i64,
    pub payload: String,
}

impl EntryRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            session_id: row.get("session_id")?,
            seq: row.get("seq")?,
            id: row.get("id")?,
            parent_id: row.get("parent_id")?,
            entry_type: row.get("type")?,
            timestamp: row.get("timestamp")?,
            payload: row.get("payload")?,
        })
    }
}

/// A row about to be inserted.
#[derive(Debug, Clone)]
pub struct NewEntryRow {
    pub seq: i64,
    pub id: String,
    pub parent_id: Option<String>,
    pub entry_type: String,
    pub timestamp: i64,
    pub payload: String,
}

/// The stored `payload` column: the entry minus its envelope
/// (`type`, `id`, `parentId`, `seq`, `timestamp`). Port of `entryPayload`.
pub fn entry_payload(entry: &Entry) -> SessionResult<Map<String, Value>> {
    let value = serde_json::to_value(entry.to_provisioned()).map_err(|error| {
        SessionError::invalid_payload(format!("Durable payload is not serializable: {error}"))
    })?;
    let mut map = match value {
        Value::Object(map) => map,
        other => {
            return Err(SessionError::invalid_payload(format!(
                "Entry payload is not an object: {other}"
            )))
        }
    };
    map.remove("type");
    map.remove("id");
    Ok(map)
}

/// Rebuilds an [`Entry`] from a stored row.
///
/// Upstream hand-validates every payload variant and wraps any failure in
/// `invalid_entry` with this exact message shape; `serde` does the validating
/// here, and the message is preserved because the upstream tests assert on it.
pub fn decode_entry(row: &EntryRow) -> SessionResult<Entry> {
    decode_entry_parts(
        &row.id,
        row.seq,
        row.parent_id.as_deref(),
        &row.entry_type,
        row.timestamp,
        &row.payload,
    )
}

pub fn decode_entry_parts(
    id: &str,
    seq: i64,
    parent_id: Option<&str>,
    entry_type: &str,
    timestamp: i64,
    payload: &str,
) -> SessionResult<Entry> {
    let invalid = || {
        SessionError::invalid_entry(format!(
            "Invalid SQLite session entry {id}: failed to decode entry {id}"
        ))
    };

    let parsed: Value = serde_json::from_str(payload).map_err(|_| invalid())?;
    let mut map = match parsed {
        Value::Object(map) => map,
        _ => return Err(invalid()),
    };
    // The envelope is rebuilt from the columns, in upstream's key order.
    let mut line = Map::new();
    line.insert("type".into(), Value::String(entry_type.to_string()));
    line.insert("id".into(), Value::String(id.to_string()));
    line.append(&mut map);
    line.insert(
        "parentId".into(),
        match parent_id {
            Some(parent) => Value::String(parent.to_string()),
            None => Value::Null,
        },
    );
    line.insert("seq".into(), Value::from(seq));
    line.insert("timestamp".into(), Value::from(timestamp));
    serde_json::from_value::<Entry>(Value::Object(line)).map_err(|_| invalid())
}

pub fn insert_entry_row(
    conn: &Connection,
    session_id: &str,
    entry: &NewEntryRow,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "INSERT INTO entries (session_id, id, seq, parent_id, type, timestamp, payload)\n\t\tVALUES (",
    );
    query
        .bind(session_id)
        .push(", ")
        .bind(entry.id.as_str())
        .push(", ")
        .bind(entry.seq)
        .push(", ")
        .bind(entry.parent_id.clone())
        .push(", ")
        .bind(entry.entry_type.as_str())
        .push(", ")
        .bind(entry.timestamp)
        .push(", ")
        .bind(entry.payload.as_str())
        .push(")");
    query.run(conn)?;
    Ok(())
}

pub fn read_entry_row(
    conn: &Connection,
    session_id: &str,
    entry_id: &str,
) -> SessionResult<Option<EntryRow>> {
    let mut query = SqlQuery::raw(
        "SELECT session_id, seq, id, parent_id, type, timestamp, payload\n\t\tFROM entries\n\t\tWHERE session_id = ",
    );
    query.bind(session_id).push(" AND id = ").bind(entry_id);
    query.get(conn, EntryRow::from_row)
}

/// Filters for [`read_entry_rows`]. Mirrors upstream's options bag: `after_seq`
/// is the log cursor (always `>`), `cursor` is the query cursor (direction
/// dependent).
#[derive(Debug, Clone, Default)]
pub struct EntryRowQuery {
    pub after_seq: Option<i64>,
    pub cursor: Option<i64>,
    pub entry_type: Option<EntryType>,
    pub order: Option<EntryOrder>,
    pub limit: Option<i64>,
}

pub fn read_entry_rows(
    conn: &Connection,
    session_id: &str,
    options: &EntryRowQuery,
) -> SessionResult<Vec<EntryRow>> {
    let oldest_first = options.order.unwrap_or_default().is_oldest_first();
    let mut query = SqlQuery::raw(
        "SELECT session_id, seq, id, parent_id, type, timestamp, payload\n\t\tFROM entries\n\t\tWHERE session_id = ",
    );
    query.bind(session_id);
    if let Some(after_seq) = options.after_seq {
        query.push(" AND seq > ").bind(after_seq);
    }
    if let Some(cursor) = options.cursor {
        query
            .push(if oldest_first {
                " AND seq > "
            } else {
                " AND seq < "
            })
            .bind(cursor);
    }
    if let Some(entry_type) = options.entry_type {
        query.push(" AND type = ").bind(entry_type.as_str());
    }
    query.push("\n\t\tORDER BY seq ");
    query.push(if oldest_first { "ASC" } else { "DESC" });
    if let Some(limit) = options.limit {
        query.push(" LIMIT ").bind(limit);
    }
    query.all(conn, EntryRow::from_row)
}

pub fn id_exists_in_entries(conn: &Connection, session_id: &str, id: &str) -> SessionResult<bool> {
    let mut query = SqlQuery::raw("SELECT 1 AS found FROM entries WHERE session_id = ");
    query.bind(session_id).push(" AND id = ").bind(id);
    query.push(" LIMIT 1");
    query.exists(conn)
}

pub fn delete_entry_rows(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM entries WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}
