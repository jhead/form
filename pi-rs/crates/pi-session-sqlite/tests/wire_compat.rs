//! Cross-implementation wire compatibility.
//!
//! Upstream has no equivalent test — it only ever talks to itself. The port
//! needs one, because "a database written by the TypeScript implementation must
//! open here and vice versa" is a hard requirement rather than a nicety. The
//! first case hand-builds rows exactly as the TypeScript backend writes them
//! and reads them back through the repository; the second appends through the
//! repository and asserts the raw column values.

mod common;

use common::*;
use pi_session::{
    EntryQuery, LogItem, LogOptions, NewRecord, OperationFinishedRecord, OperationOutcome,
    RecordPayload, RecordQuery, SessionMetadata,
};
use pi_session_sqlite::apply_migrations;
use rusqlite::{params, Connection};
use serde_json::json;

/// Exactly what `JSON.stringify` produces for a v4 message entry payload.
const TS_MESSAGE_PAYLOAD: &str = r#"{"message":{"role":"user","content":[{"type":"text","text":"written by pi (TypeScript)"}],"timestamp":1730000000000}}"#;
/// A whole provisioned record, the way `appendRecordRow` stores it.
const TS_RECORD_PAYLOAD: &str = r#"{"type":"operation_finished","id":"record-1","lane":"main","runId":"run-1","outcome":"completed"}"#;

fn write_typescript_database(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    apply_migrations(&conn).unwrap();
    conn.execute(
        "INSERT INTO sessions (id, created_at, metadata, cwd, parent_session_id) VALUES (?, ?, ?, ?, ?)",
        params![
            "ts-session",
            1_730_000_000_000i64,
            r#"{"profile":"reviewer"}"#,
            "/workspace/project",
            "ts-parent"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_sequences (session_id, next_seq) VALUES (?, ?)",
        params!["ts-session", 4i64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_stats (session_id, message_count, cached_tokens, uncached_tokens, total_tokens, cost_total)
         VALUES (?, ?, ?, ?, ?, ?)",
        params!["ts-session", 1i64, 3.0f64, 12.0f64, 20.0f64, 1.5f64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO entries (session_id, id, seq, parent_id, type, timestamp, payload) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            "ts-session",
            "entry-1",
            1i64,
            Option::<String>::None,
            "message",
            1_730_000_000_001i64,
            TS_MESSAGE_PAYLOAD
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO lanes (session_id, lane, leaf_id, open_operation_id) VALUES (?, ?, ?, NULL)",
        params!["ts-session", "main", "entry-1"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO branch_entries (session_id, branch_id, entry_id, entry_seq, entry_type, custom_type)
         VALUES (?, ?, ?, ?, ?, NULL)",
        params!["ts-session", "branch-1", "entry-1", 1i64, "message"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO branch_tips (session_id, branch_id, tip_id) VALUES (?, ?, ?)",
        params!["ts-session", "branch-1", "entry-1"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO records (session_id, seq, id, lane, run_id, type, op_kind, timestamp, payload)
         VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)",
        params![
            "ts-session",
            2i64,
            "record-1",
            "main",
            "run-1",
            "operation_finished",
            1_730_000_000_002i64,
            TS_RECORD_PAYLOAD
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO facts (session_id, seq, kind, key, value) VALUES (?, ?, ?, NULL, ?)",
        params!["ts-session", 3i64, "name", r#""Written upstream""#],
    )
    .unwrap();
    conn.close().unwrap();
}

#[tokio::test]
async fn opens_a_database_written_by_the_typescript_backend() {
    let fixture = Fixture::new();
    write_typescript_database(&fixture.database_path());

    // The metadata the repository lists is enough to open the session.
    let listed = fixture.repo.list(&Default::default()).await.unwrap();
    assert_eq!(listed.len(), 1);
    let metadata = &listed[0];
    assert_eq!(metadata.id, "ts-session");
    assert_eq!(metadata.created_at, 1_730_000_000_000);
    assert_eq!(metadata.parent_session_id.as_deref(), Some("ts-parent"));
    assert_eq!(metadata.get_str("cwd"), Some("/workspace/project"));
    assert_eq!(metadata.get_str("name"), Some("Written upstream"));
    assert_eq!(
        metadata.get("metadata"),
        Some(&json!({ "profile": "reviewer" }))
    );

    let session = fixture.repo.open(metadata).await.unwrap();
    let entries = session.find_entries(&EntryQuery::new()).await.unwrap();
    assert_eq!(ids(&entries), vec!["entry-1"]);
    assert_eq!(
        entries[0].as_message().unwrap().message,
        user_message_at("written by pi (TypeScript)", 1_730_000_000_000)
    );
    assert_eq!(
        session.get_name().await.unwrap().as_deref(),
        Some("Written upstream")
    );

    let records = session.find_records(&RecordQuery::new()).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id(), Some("run-1"));

    // The log stitches all four tables back together in commit order.
    let log = session.get_log(&LogOptions::default()).await.unwrap();
    assert_eq!(
        log.iter()
            .map(|item| (item.kind(), item.seq()))
            .collect::<Vec<_>>(),
        vec![("entry", 1), ("record", 2), ("fact", 3)]
    );
    assert!(matches!(log[2], LogItem::Name { .. }));

    // And the shared sequence continues where upstream left it.
    let appended = session
        .append_message(user_message("continued in Rust"))
        .await
        .unwrap();
    let entry = session.get_entry(&appended).await.unwrap().unwrap();
    assert_eq!(
        (entry.seq, entry.parent_id.as_deref()),
        (4, Some("entry-1"))
    );
}

#[tokio::test]
async fn writes_rows_the_typescript_backend_can_read() {
    let fixture = Fixture::new();
    let session = fixture.create("rs-session").await;
    let entry_id = session
        .append_message(user_message_at("written by pi-rs", 1_730_000_000_000))
        .await
        .unwrap();
    session
        .append_record(&NewRecord::new(
            "record-1",
            "main",
            RecordPayload::OperationFinished(OperationFinishedRecord {
                run_id: "run-1".into(),
                outcome: OperationOutcome::Completed,
                error: None,
            }),
        ))
        .await
        .unwrap();
    session.set_name(Some("Written here")).await.unwrap();
    session
        .set_label(&entry_id, Some("checkpoint"))
        .await
        .unwrap();

    fixture.inspect(|conn| {
        // The entry payload is the entry minus its envelope, camelCase, with
        // no storage-assigned fields duplicated inside it.
        let (payload, entry_type, parent_id, seq): (String, String, Option<String>, i64) = conn
            .query_row(
                "SELECT payload, type, parent_id, seq FROM entries WHERE session_id = ? AND id = ?",
                params!["rs-session", entry_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            payload,
            r#"{"message":{"role":"user","content":[{"type":"text","text":"written by pi-rs"}],"timestamp":1730000000000}}"#
        );
        assert_eq!((entry_type.as_str(), parent_id, seq), ("message", None, 1));

        // The record payload is the whole provisioned record; run_id and
        // op_kind are denormalized columns, not payload fields.
        let (payload, run_id, op_kind): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT payload, run_id, op_kind FROM records WHERE session_id = ? AND id = ?",
                params!["rs-session", "record-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(payload, TS_RECORD_PAYLOAD);
        assert_eq!((run_id.as_deref(), op_kind), (Some("run-1"), None));

        // Facts store JSON, so a name is a quoted string.
        let facts: Vec<(i64, String, Option<String>, Option<String>)> = conn
            .prepare("SELECT seq, kind, key, value FROM facts WHERE session_id = ? ORDER BY seq")
            .unwrap()
            .query_map(["rs-session"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            facts,
            vec![
                (3, "name".to_string(), None, Some(r#""Written here""#.to_string())),
                (
                    4,
                    "label".to_string(),
                    Some(entry_id.clone()),
                    Some(r#""checkpoint""#.to_string())
                ),
            ]
        );

        // The migration ledger is what makes the two implementations agree that
        // the schema is already current.
        let applied: Vec<String> = conn
            .prepare("SELECT id FROM migrations ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(applied, vec!["001_initial.sql".to_string()]);
    });
}

#[tokio::test]
async fn a_session_written_here_reopens_from_bare_metadata() {
    let fixture = Fixture::new();
    let session = fixture.create("rs-session").await;
    let entry_id = session
        .append_message(user_message("persisted"))
        .await
        .unwrap();
    fixture.repo.close().await.unwrap();

    // Only the id survives a round trip through, say, a protocol boundary.
    let bare = SessionMetadata::new("rs-session", 0);
    let repo = fixture.peer_repo(Default::default());
    let reopened = repo.open(&bare).await.unwrap();
    assert_eq!(ids(&all_entries(&reopened).await.unwrap()), vec![entry_id]);
}

fn user_message_at(text: &str, timestamp: i64) -> pi_session::AgentMessage {
    pi_session::AgentMessage::User(pi_core::UserMessage {
        content: pi_core::UserContent::Blocks(vec![pi_core::InputContent::text(text)]),
        timestamp,
    })
}
