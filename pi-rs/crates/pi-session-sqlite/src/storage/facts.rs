//! `facts` table access. Port of `sqlite/storage/facts.ts`.
//!
//! Facts are the append-only projection behind `getName`/`getLabel`: latest seq
//! wins, and a `NULL` value clears. Values are stored JSON-encoded (so a name
//! is `"text"`, quotes included) exactly as upstream writes them.

use rusqlite::Connection;

use pi_session::SessionResult;

use crate::sql::SqlQuery;

#[derive(Debug, Clone)]
pub struct FactRow {
    pub seq: i64,
    pub kind: String,
    pub key: Option<String>,
    pub value: Option<String>,
}

impl FactRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get("seq")?,
            kind: row.get("kind")?,
            key: row.get("key")?,
            value: row.get("value")?,
        })
    }
}

pub fn append_fact(
    conn: &Connection,
    session_id: &str,
    seq: i64,
    kind: &str,
    key: Option<&str>,
    value: Option<&str>,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw("INSERT INTO facts (session_id, seq, kind, key, value) VALUES (");
    query
        .bind(session_id)
        .push(", ")
        .bind(seq)
        .push(", ")
        .bind(kind)
        .push(", ")
        .bind(key)
        .push(", ")
        .bind(value)
        .push(")");
    query.run(conn)?;
    Ok(())
}

pub fn read_latest_fact(
    conn: &Connection,
    session_id: &str,
    kind: &str,
    key: Option<&str>,
) -> SessionResult<Option<FactRow>> {
    let mut query = SqlQuery::raw(
        "SELECT session_id, seq, kind, key, value\n\t\tFROM facts INDEXED BY idx_facts_session_kind_key_seq\n\t\tWHERE session_id = ",
    );
    query
        .bind(session_id)
        .push(" AND kind = ")
        .bind(kind)
        .push(" AND key IS ")
        .bind(key)
        .push("\n\t\tORDER BY seq DESC\n\t\tLIMIT 1");
    query.get(conn, FactRow::from_row)
}

/// The latest still-set label for every target, ordered by target id.
pub fn read_latest_label_facts(
    conn: &Connection,
    session_id: &str,
) -> SessionResult<Vec<(String, String)>> {
    let mut query = SqlQuery::raw(
        "SELECT f.key, f.value
\t\tFROM facts AS f INDEXED BY idx_facts_session_kind_key_seq
\t\tWHERE f.session_id = ",
    );
    query.bind(session_id).push(
        "
\t\t\tAND f.kind = 'label'
\t\t\tAND f.value IS NOT NULL
\t\t\tAND f.seq = (
\t\t\t\tSELECT MAX(candidate.seq)
\t\t\t\tFROM facts AS candidate INDEXED BY idx_facts_session_kind_key_seq
\t\t\t\tWHERE candidate.session_id = f.session_id
\t\t\t\t\tAND candidate.kind = f.kind
\t\t\t\t\tAND candidate.key IS f.key
\t\t\t)
\t\tORDER BY f.key",
    );
    query.all(conn, |row| Ok((row.get("key")?, row.get("value")?)))
}

pub fn read_fact_rows(
    conn: &Connection,
    session_id: &str,
    after_seq: Option<i64>,
    limit: Option<i64>,
) -> SessionResult<Vec<FactRow>> {
    let mut query = SqlQuery::raw(
        "SELECT session_id, seq, kind, key, value\n\t\tFROM facts\n\t\tWHERE session_id = ",
    );
    query.bind(session_id);
    if let Some(after_seq) = after_seq {
        query.push(" AND seq > ").bind(after_seq);
    }
    query.push("\n\t\tORDER BY seq");
    if let Some(limit) = limit {
        query.push(" LIMIT ").bind(limit);
    }
    query.all(conn, FactRow::from_row)
}

pub fn delete_fact_rows(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM facts WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}

/// Facts store JSON, so a name/label round-trips through `JSON.parse`.
pub fn decode_fact_string(
    value: Option<&str>,
    session_id: &str,
    what: &str,
) -> SessionResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(value).map_err(|_| {
        pi_session::SessionError::storage(format!(
            "Invalid SQLite session {session_id}: {what} is not valid JSON"
        ))
    })?;
    match parsed {
        serde_json::Value::String(text) => Ok(Some(text)),
        _ => Err(pi_session::SessionError::storage(format!(
            "Invalid SQLite session {session_id}: {what} must be a string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migrations;

    /// Port of `facts-query.test.ts`.
    #[test]
    fn reads_latest_facts_and_latest_non_null_labels() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn).unwrap();
        let json = |text: &str| serde_json::to_string(text).unwrap();
        append_fact(
            &conn,
            "session-1",
            1,
            "label",
            Some("entry-1"),
            Some(&json("old")),
        )
        .unwrap();
        append_fact(
            &conn,
            "session-1",
            2,
            "label",
            Some("entry-2"),
            Some(&json("kept")),
        )
        .unwrap();
        append_fact(
            &conn,
            "session-1",
            3,
            "label",
            Some("entry-1"),
            Some(&json("new")),
        )
        .unwrap();
        append_fact(
            &conn,
            "session-1",
            4,
            "label",
            Some("entry-3"),
            Some(&json("removed")),
        )
        .unwrap();
        append_fact(&conn, "session-1", 5, "label", Some("entry-3"), None).unwrap();
        append_fact(
            &conn,
            "session-1",
            6,
            "name",
            None,
            Some(&json("session name")),
        )
        .unwrap();
        append_fact(
            &conn,
            "other-session",
            1,
            "label",
            Some("entry-1"),
            Some(&json("other")),
        )
        .unwrap();

        assert_eq!(
            read_latest_fact(&conn, "session-1", "label", Some("entry-1"))
                .unwrap()
                .and_then(|row| row.value),
            Some(json("new"))
        );
        assert_eq!(
            read_latest_fact(&conn, "session-1", "name", None)
                .unwrap()
                .and_then(|row| row.value),
            Some(json("session name"))
        );
        assert_eq!(
            read_latest_label_facts(&conn, "session-1").unwrap(),
            vec![
                ("entry-1".to_string(), json("new")),
                ("entry-2".to_string(), json("kept")),
            ]
        );
    }
}
