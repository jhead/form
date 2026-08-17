//! JSONL v4 wire-compatibility fixture.
//!
//! `SESSION_JSONL` below is hand-written to match exactly what the TypeScript
//! implementation emits, derived from `session/jsonl/codec.ts`
//! (`encodeHeader` / `encodeMutation`) plus the object-literal key order of the
//! upstream call sites in `session/storage.ts` and `session/session.ts`:
//!
//! ```text
//! entry line  = { kind, lane?, ...provisioned, parentId, seq, timestamp }
//! record line = { kind, ...newRecord, seq, timestamp }
//! ```
//!
//! The test decodes every line, re-encodes it and asserts the bytes are
//! identical, then loads the whole file through the real storage to prove the
//! semantics survive too.
//!
//! ## Known representation divergence
//!
//! `Usage.cost` is `f64`. `serde_json` writes an integral `f64` as `1.0` while
//! `JSON.stringify` writes `1`. Both parse to the same number, so files remain
//! *loadable* in both directions, but they are not byte-identical for integral
//! costs. The fixture therefore uses fractional costs — which is what real
//! sessions contain — and `integral_costs_differ_in_representation_only` pins
//! the divergence explicitly.

use std::sync::Arc;

use pi_session::jsonl::codec::{encode_header, encode_mutation, parse_header, parse_mutation};
use pi_session::repo::{BranchStore, EntryStore, SessionRepo};
use pi_session::types::{
    EntryOrder, EntryQuery, EntryType, LanePointer, LogOptions, SessionCreateOptions,
    SessionListOptions,
};
use pi_session::{JsonlSessionRepo, JsonlSessionStorage};

/// A complete v4 session as the TypeScript implementation would write it.
const SESSION_JSONL: &str = concat!(
    r#"{"kind":"header","version":4,"id":"wire-fixture","createdAt":1700000000000,"cwd":"/workspace/project","parentSessionId":"parent","metadata":{"owner":"agent"}}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"read /tmp/a.txt"}],"timestamp":1700000000001},"parentId":null,"seq":1,"timestamp":1700000000001}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"On it."},{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"/tmp/a.txt"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":120,"output":45,"cacheRead":30,"cacheWrite":12,"totalTokens":207,"cost":{"input":0.25,"output":0.5,"cacheRead":0.125,"cacheWrite":0.0625,"total":0.9375}},"stopReason":"toolUse","timestamp":1700000000002},"parentId":"u1","seq":2,"timestamp":1700000000002}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"message","id":"t1","message":{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[{"type":"text","text":"file body"}],"isError":false,"timestamp":1700000000003},"terminate":true,"parentId":"a1","seq":3,"timestamp":1700000000003}"#,
    "\n",
    r#"{"kind":"record","type":"operation_started","id":"run-1","lane":"main","sourceLeafId":"a1","intent":{"kind":"run","originalPrompt":[],"initialMessages":[]},"seq":4,"timestamp":1700000000004}"#,
    "\n",
    r#"{"kind":"record","type":"step_attempt","id":"att-1","lane":"main","runId":"run-1","step":"assistant","attempt":1,"resultEntryId":"a1","seq":5,"timestamp":1700000000005}"#,
    "\n",
    r#"{"kind":"record","type":"usage","id":"use-1","lane":"main","cause":"assistant","runId":"run-1","entryId":"a1","attempt":1,"stopReason":"toolUse","usage":{"input":120,"output":45,"cacheRead":30,"cacheWrite":12,"totalTokens":207,"cost":{"input":0.25,"output":0.5,"cacheRead":0.125,"cacheWrite":0.0625,"total":0.9375}},"seq":6,"timestamp":1700000000006}"#,
    "\n",
    r#"{"kind":"lane","seq":7,"lane":"thread","leafId":"a1"}"#,
    "\n",
    r#"{"kind":"entry","lane":"thread","type":"custom","id":"c1","customType":"note","data":{"value":1},"futureField":{"added":"by a newer pi"},"parentId":"a1","seq":8,"timestamp":1700000000008}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"compaction","id":"cmp","summary":"Goal\nRead the file.","retainedTail":[{"role":"user","content":[{"type":"text","text":"and then?"}],"timestamp":1700000000009}],"tokensBefore":4096,"details":{"readFiles":["/tmp/a.txt"],"modifiedFiles":[]},"parentId":"t1","seq":9,"timestamp":1700000000009}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"branch_summary","id":"bs","fromId":"c1","summary":"explored a side branch","parentId":"cmp","seq":10,"timestamp":1700000000010}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"model_change","id":"mc","provider":"anthropic","modelId":"claude-opus-4-1","parentId":"bs","seq":11,"timestamp":1700000000011}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"thinking_level_change","id":"tl","thinkingLevel":"high","parentId":"mc","seq":12,"timestamp":1700000000012}"#,
    "\n",
    r#"{"kind":"entry","lane":"main","type":"active_tools_change","id":"at","activeToolNames":["bash","read"],"parentId":"tl","seq":13,"timestamp":1700000000013}"#,
    "\n",
    r#"{"kind":"fact","seq":14,"fact":"name","name":"Example"}"#,
    "\n",
    r#"{"kind":"fact","seq":15,"fact":"name"}"#,
    "\n",
    r#"{"kind":"fact","seq":16,"fact":"label","targetId":"u1","label":"checkpoint"}"#,
    "\n",
    r#"{"kind":"record","type":"queue_enqueued","id":"q1","lane":"main","queue":"nextRun","target":{"type":"message","id":"qm","message":{"role":"user","content":[{"type":"text","text":"queued"}],"timestamp":1700000000017}},"seq":17,"timestamp":1700000000017}"#,
    "\n",
    r#"{"kind":"record","type":"operation_finished","id":"fin-1","lane":"main","runId":"run-1","outcome":"completed","seq":18,"timestamp":1700000000018}"#,
    "\n",
    r#"{"kind":"entry","type":"custom","id":"imported","customType":"imported","parentId":"at","seq":19,"timestamp":1700000000019}"#,
    "\n",
    r#"{"kind":"lane","seq":20,"lane":"main","leafId":"imported"}"#,
    "\n",
);

/// Decode then re-encode every line and rebuild the file.
fn reencode(source: &str) -> String {
    let mut lines = source.split('\n').collect::<Vec<_>>();
    assert_eq!(
        lines.pop(),
        Some(""),
        "a v4 session file ends with a newline"
    );
    let header = parse_header(lines[0]).expect("header decodes");
    let mut rebuilt = encode_header(&header);
    for line in &lines[1..] {
        let mutation = parse_mutation(line)
            .unwrap_or_else(|error| panic!("line {line} failed to decode: {error}"));
        rebuilt.push_str(&encode_mutation(&mutation).expect("mutation encodes"));
    }
    rebuilt
}

#[test]
fn upstream_session_file_round_trips_byte_for_byte() {
    assert_eq!(reencode(SESSION_JSONL), SESSION_JSONL);
}

#[test]
fn unknown_fields_survive_the_round_trip() {
    // `futureField` is not part of the v4 schema this crate knows about; a file
    // written by a newer upstream must not lose it.
    assert!(SESSION_JSONL.contains(r#""futureField":{"added":"by a newer pi"}"#));
    assert!(reencode(SESSION_JSONL).contains(r#""futureField":{"added":"by a newer pi"}"#));
}

#[tokio::test]
async fn upstream_session_file_loads_with_its_semantics_intact() {
    let root = tempfile::tempdir().unwrap();
    let path = root
        .path()
        .join("2023-11-14T22-13-20-000Z_wire-fixture.jsonl");
    tokio::fs::write(&path, SESSION_JSONL).await.unwrap();

    let storage = JsonlSessionStorage::load(&path)
        .await
        .expect("fixture loads");
    let metadata = storage.metadata().clone();
    assert_eq!(metadata.id, "wire-fixture");
    assert_eq!(metadata.created_at, 1_700_000_000_000);
    assert_eq!(metadata.parent_session_id.as_deref(), Some("parent"));
    assert_eq!(metadata.get_str("cwd"), Some("/workspace/project"));
    assert_eq!(metadata.get("sourceFormat"), Some(&serde_json::json!(4)));
    assert_eq!(
        metadata.get("metadata"),
        Some(&serde_json::json!({ "owner": "agent" }))
    );

    assert_eq!(
        storage.get_lanes().await.unwrap(),
        vec![
            LanePointer {
                lane: "main".into(),
                leaf_id: Some("imported".into())
            },
            // `c1` was appended to `thread`, so that lane's leaf moved past `a1`.
            LanePointer {
                lane: "thread".into(),
                leaf_id: Some("c1".into())
            },
        ]
    );

    // Facts: the name was set then cleared, the label survives.
    assert_eq!(storage.get_name().await.unwrap(), None);
    assert_eq!(
        storage.get_label("u1").await.unwrap().as_deref(),
        Some("checkpoint")
    );

    // Statistics come from the one usage record, not from message usage.
    let stats = storage.get_stats().await.unwrap();
    assert_eq!(stats.message_count, 3);
    assert_eq!(stats.cached_tokens, 30);
    assert_eq!(stats.uncached_tokens, 132);
    assert_eq!(stats.total_tokens, 207);
    assert!((stats.cost_total - 0.9375).abs() < f64::EPSILON);

    // Every entry type decoded into its typed payload.
    let entries = storage
        .find_entries(&EntryQuery::new().with_order(EntryOrder::OldestFirst))
        .await
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.entry_type())
            .collect::<Vec<_>>(),
        vec![
            EntryType::Message,
            EntryType::Message,
            EntryType::Message,
            EntryType::Custom,
            EntryType::Compaction,
            EntryType::BranchSummary,
            EntryType::ModelChange,
            EntryType::ThinkingLevelChange,
            EntryType::ActiveToolsChange,
            EntryType::Custom,
        ]
    );
    let compaction = entries[4].as_compaction().unwrap();
    assert_eq!(compaction.tokens_before, 4096);
    assert_eq!(compaction.retained_tail.len(), 1);
    assert_eq!(
        compaction.details,
        Some(serde_json::json!({ "readFiles": ["/tmp/a.txt"], "modifiedFiles": [] }))
    );

    // The queue_enqueued record's nested provisioned target decoded too, and the
    // operation it belongs to is closed.
    assert_eq!(
        storage.get_log(&LogOptions::default()).await.unwrap().len(),
        20
    );
    assert!(storage
        .find_open_operations("main", Some(2))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn files_this_crate_writes_reparse_identically() {
    let root = tempfile::tempdir().unwrap();
    let repository = JsonlSessionRepo::new(root.path());
    let session = repository
        .create(
            &SessionCreateOptions::new()
                .with_id("written")
                .with_cwd(root.path().display().to_string()),
        )
        .await
        .unwrap();

    let user = pi_session::messages::AgentMessage::user_text("hello");
    session.append_message(user).await.unwrap();
    session
        .append_custom_entry("note", Some(serde_json::json!({ "n": 1 })))
        .await
        .unwrap();
    session.set_name(Some("Written")).await.unwrap();
    session.drain().await.unwrap();

    let metadata = session.get_metadata().await.unwrap();
    let path = metadata.get_str("path").unwrap().to_string();
    let content = tokio::fs::read_to_string(&path).await.unwrap();

    assert!(content.ends_with('\n'), "every line is newline-terminated");
    assert_eq!(
        reencode(&content),
        content,
        "our own output must be codec-stable"
    );

    // And the file is listable and reopenable through the repository.
    let listed = repository
        .list(&SessionListOptions::default())
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let reopened = repository.open(&listed[0]).await.unwrap();
    assert_eq!(
        reopened.get_name().await.unwrap().as_deref(),
        Some("Written")
    );
    assert_eq!(
        reopened
            .find_entries(&EntryQuery::new())
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn a_torn_final_line_is_repaired_on_load() {
    // An append that never reached disk in full leaves a partial JSON tail.
    // Upstream drops it by republishing the valid prefix; so must this port.
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("torn.jsonl");
    let torn = format!("{SESSION_JSONL}{{\"kind\":\"entry\",\"lane\":\"ma");
    tokio::fs::write(&path, &torn).await.unwrap();

    let storage = JsonlSessionStorage::load(&path)
        .await
        .expect("torn tail is repaired");
    assert_eq!(
        storage.get_log(&LogOptions::default()).await.unwrap().len(),
        20
    );
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        SESSION_JSONL
    );
}

#[tokio::test]
async fn an_unterminated_final_line_is_completed_on_load() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("unterminated.jsonl");
    tokio::fs::write(&path, SESSION_JSONL.trim_end_matches('\n'))
        .await
        .unwrap();

    let storage = JsonlSessionStorage::load(&path).await.expect("loads");
    assert_eq!(
        storage.get_log(&LogOptions::default()).await.unwrap().len(),
        20
    );
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        SESSION_JSONL
    );
}

#[tokio::test]
async fn a_corrupt_interior_line_fails_the_load() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("corrupt.jsonl");
    let mut lines: Vec<&str> = SESSION_JSONL.split('\n').collect();
    lines[3] =
        r#"{"kind":"entry","lane":"main","type":"message","id":"broken","seq":3,"timestamp":1}"#;
    tokio::fs::write(&path, lines.join("\n")).await.unwrap();

    let error = match JsonlSessionStorage::load(&path).await {
        Err(error) => error,
        Ok(_) => panic!("a corrupt interior line must fail the load"),
    };
    assert_eq!(error.code(), "invalid_entry");
    assert!(error.message().contains("line 4"), "{error}");
}

#[test]
fn integral_costs_differ_in_representation_only() {
    // Documented divergence: `JSON.stringify` writes `1`, `serde_json` writes
    // `1.0`. Both round-trip through either implementation as the same number.
    let usage = pi_core::Usage {
        cost: pi_core::Cost {
            total: 1.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let encoded = serde_json::to_string(&usage).unwrap();
    assert!(encoded.contains(r#""total":1.0"#), "{encoded}");
    let decoded: pi_core::Usage = serde_json::from_str(&encoded.replace("1.0", "1")).unwrap();
    assert_eq!(decoded.cost.total, 1.0);
}

/// The scanning search backend is generic over any `SessionStorage`, so it must
/// work against a session loaded straight off disk.
#[tokio::test]
async fn scanning_search_runs_against_a_loaded_session() {
    use pi_session::repo::{SearchBackend, SessionSearchOptions};
    use pi_session::search::ScanningSessionSearch;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("search.jsonl");
    tokio::fs::write(&path, SESSION_JSONL).await.unwrap();
    let storage = Arc::new(JsonlSessionStorage::load(&path).await.unwrap());

    let search = ScanningSessionSearch::new(vec![storage]);
    let hits = search
        .search("/tmp/a.txt", &SessionSearchOptions::default())
        .await
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|hit| hit.session_id == "wire-fixture"));

    let bounded = search
        .search(
            "/tmp/a.txt",
            &SessionSearchOptions {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(bounded.len(), 1);
}
