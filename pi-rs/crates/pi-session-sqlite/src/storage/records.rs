//! `records` table access. Port of `sqlite/storage/records.ts`.

use rusqlite::Connection;
use serde_json::{Map, Value};

use pi_session::{
    EntryOrder, LaneRecord, NewRecord, OperationKind, RecordPayload, SessionError, SessionResult,
};

use crate::sql::{join_sql_fragments, SqlQuery};

/// A row of `records`.
#[derive(Debug, Clone)]
pub struct RecordRow {
    pub session_id: String,
    pub seq: i64,
    pub id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub record_type: String,
    pub op_kind: Option<String>,
    pub timestamp: i64,
    pub payload: String,
}

impl RecordRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            session_id: row.get("session_id")?,
            seq: row.get("seq")?,
            id: row.get("id")?,
            lane: row.get("lane")?,
            run_id: row.get("run_id")?,
            record_type: row.get("type")?,
            op_kind: row.get("op_kind")?,
            timestamp: row.get("timestamp")?,
            payload: row.get("payload")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewRecordRow {
    pub seq: i64,
    pub id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub record_type: String,
    pub op_kind: Option<String>,
    pub timestamp: i64,
    pub payload: String,
}

/// `recordRunId` — an `operation_started` record *is* its own operation.
pub fn record_run_id(record: &NewRecord) -> Option<String> {
    match &record.payload {
        RecordPayload::OperationStarted(_) => Some(record.id.clone()),
        payload => payload.run_id().map(str::to_string),
    }
}

/// The `op_kind` column value for an [`OperationKind`], matching the
/// discriminant upstream writes (`intent.kind`).
pub fn operation_kind_column(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Run => "run",
        OperationKind::Compaction => "compaction",
        OperationKind::Navigation => "navigation",
    }
}

/// `recordOpKind` — only `operation_started` rows carry one.
pub fn record_op_kind(record: &NewRecord) -> Option<String> {
    match &record.payload {
        RecordPayload::OperationStarted(started) => {
            Some(operation_kind_column(started.intent.kind()).to_string())
        }
        _ => None,
    }
}

/// Rebuilds a [`LaneRecord`]. The stored payload is the whole provisioned
/// record, so only `seq` and `timestamp` are re-attached.
pub fn decode_record(seq: i64, timestamp: i64, payload: &str) -> SessionResult<LaneRecord> {
    let invalid = || {
        SessionError::storage(format!(
            "Invalid SQLite session record at sequence {seq}: failed to decode payload"
        ))
    };
    let parsed: Value = serde_json::from_str(payload).map_err(|_| invalid())?;
    let mut map: Map<String, Value> = match parsed {
        Value::Object(map) => map,
        _ => return Err(invalid()),
    };
    map.insert("seq".into(), Value::from(seq));
    map.insert("timestamp".into(), Value::from(timestamp));
    serde_json::from_value::<LaneRecord>(Value::Object(map)).map_err(|_| invalid())
}

pub fn decode_record_row(row: &RecordRow) -> SessionResult<LaneRecord> {
    decode_record(row.seq, row.timestamp, &row.payload)
}

pub fn append_record_row(
    conn: &Connection,
    session_id: &str,
    record: &NewRecordRow,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "INSERT INTO records\n\t\t\t(session_id, seq, id, lane, run_id, type, op_kind, timestamp, payload)\n\t\t\tVALUES (",
    );
    query
        .bind(session_id)
        .push(", ")
        .bind(record.seq)
        .push(", ")
        .bind(record.id.as_str())
        .push(", ")
        .bind(record.lane.as_str())
        .push(", ")
        .bind(record.run_id.clone())
        .push(", ")
        .bind(record.record_type.as_str())
        .push(", ")
        .bind(record.op_kind.clone())
        .push(", ")
        .bind(record.timestamp)
        .push(", ")
        .bind(record.payload.as_str())
        .push(")");
    query.run(conn)?;
    Ok(())
}

pub fn id_exists_in_records(conn: &Connection, session_id: &str, id: &str) -> SessionResult<bool> {
    let mut query = SqlQuery::raw("SELECT 1 AS found FROM records WHERE session_id = ");
    query.bind(session_id).push(" AND id = ").bind(id);
    query.push(" LIMIT 1");
    query.exists(conn)
}

pub fn delete_record_rows(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM records WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}

/// Filters for [`read_record_rows`].
#[derive(Debug, Clone, Default)]
pub struct RecordRowQuery {
    pub lane: Option<String>,
    pub record_type: Option<String>,
    pub run_id: Option<String>,
    pub operation_kind: Option<String>,
    pub after_seq: Option<i64>,
    pub order: Option<EntryOrder>,
    pub limit: Option<i64>,
}

pub fn read_record_rows(
    conn: &Connection,
    session_id: &str,
    query: &RecordRowQuery,
) -> SessionResult<Vec<RecordRow>> {
    let mut predicates = vec![{
        let mut fragment = SqlQuery::raw("session_id = ");
        fragment.bind(session_id);
        fragment
    }];
    let mut push = |column: &str, value: &str| {
        let mut fragment = SqlQuery::raw(format!("{column} = "));
        fragment.bind(value);
        predicates.push(fragment);
    };
    if let Some(lane) = &query.lane {
        push("lane", lane);
    }
    if let Some(record_type) = &query.record_type {
        push("type", record_type);
    }
    if let Some(run_id) = &query.run_id {
        push("run_id", run_id);
    }
    if let Some(kind) = &query.operation_kind {
        push("op_kind", kind);
    }
    if let Some(after_seq) = query.after_seq {
        let mut fragment = SqlQuery::raw("seq > ");
        fragment.bind(after_seq);
        predicates.push(fragment);
    }

    let mut statement = SqlQuery::raw(
        "SELECT session_id, seq, id, lane, run_id, type, op_kind, timestamp, payload\n\t\tFROM records\n\t\tWHERE ",
    );
    statement.append(&join_sql_fragments(&predicates, " AND "));
    statement.push("\n\t\tORDER BY seq ");
    statement.push(if query.order.unwrap_or_default().is_oldest_first() {
        "ASC"
    } else {
        "DESC"
    });
    if let Some(limit) = query.limit {
        statement.push(" LIMIT ").bind(limit);
    }
    statement.all(conn, RecordRow::from_row)
}

/// The lane's open operation, if any.
///
/// Upstream ignores the `limit` option here on purpose: a lane can hold at most
/// one open operation, so the result is either empty or a single row.
pub fn read_open_operation_rows(
    conn: &Connection,
    session_id: &str,
    lane: &str,
    _limit: Option<i64>,
) -> SessionResult<Vec<RecordRow>> {
    let mut lane_query = SqlQuery::raw("SELECT open_operation_id FROM lanes WHERE session_id = ");
    lane_query.bind(session_id).push(" AND lane = ").bind(lane);
    let open_operation_id: Option<String> = lane_query
        .get(conn, |row| row.get::<_, Option<String>>(0))?
        .flatten();
    let Some(open_operation_id) = open_operation_id else {
        return Ok(Vec::new());
    };

    let mut query = SqlQuery::raw(
        "SELECT session_id, seq, id, lane, run_id, type, op_kind, timestamp, payload\n\t\tFROM records\n\t\tWHERE session_id = ",
    );
    query
        .bind(session_id)
        .push("\n\t\t\tAND id = ")
        .bind(open_operation_id.as_str());
    let record = query.get(conn, RecordRow::from_row)?.ok_or_else(|| {
        SessionError::storage(format!(
            "Lane {lane} points at missing open operation {open_operation_id}"
        ))
    })?;
    if record.lane != lane || record.record_type != "operation_started" {
        return Err(SessionError::storage(format!(
            "Lane {lane} points at invalid open operation {open_operation_id}"
        )));
    }
    Ok(vec![record])
}
