//! Port of `test/repository.test.ts` and `test/log-query.test.ts`.

mod common;

use common::*;
use pi_session::{
    ForkOptions, LogItem, LogOptions, NewRecord, OperationFinishedRecord, OperationOutcome,
    RecordPayload, RecordQuery, SessionStats, UsageCause, UsageRecord,
};
use rusqlite::params;
use serde_json::json;

#[tokio::test]
async fn persists_session_metadata_through_create_list_open_and_fork() {
    let fixture = Fixture::new();
    let mut options = fixture.create_options("session-1");
    options.metadata = Some(
        json!({ "profile": "reviewer" })
            .as_object()
            .cloned()
            .unwrap(),
    );
    let source = fixture.repo.create(&options).await.unwrap();
    let source_metadata = source.get_metadata().await.unwrap();
    assert_eq!(
        source_metadata.get("metadata"),
        Some(&json!({ "profile": "reviewer" }))
    );

    let listed = fixture
        .repo
        .list(&pi_session::SessionListOptions {
            cwd: Some(fixture.cwd()),
        })
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|metadata| metadata.get("metadata").cloned())
            .collect::<Vec<_>>(),
        vec![Some(json!({ "profile": "reviewer" }))]
    );

    let reopened = fixture.repo.open(&source_metadata).await.unwrap();
    assert_eq!(
        reopened.get_metadata().await.unwrap().get("metadata"),
        Some(&json!({ "profile": "reviewer" }))
    );

    // A fork inherits the source's application metadata...
    let fork = fixture
        .repo
        .fork(
            &source_metadata,
            &ForkOptions::default(),
            &fixture.create_options("session-2"),
        )
        .await
        .unwrap();
    assert_eq!(
        fork.get_metadata().await.unwrap().get("metadata"),
        Some(&json!({ "profile": "reviewer" }))
    );

    // ...unless the fork options override it.
    let mut overridden_options = fixture.create_options("session-3");
    overridden_options.metadata =
        Some(json!({ "profile": "writer" }).as_object().cloned().unwrap());
    let overridden = fixture
        .repo
        .fork(
            &source_metadata,
            &ForkOptions::default(),
            &overridden_options,
        )
        .await
        .unwrap();
    assert_eq!(
        overridden.get_metadata().await.unwrap().get("metadata"),
        Some(&json!({ "profile": "writer" }))
    );
}

#[tokio::test]
async fn rolls_back_the_entire_fork_when_copying_an_entry_fails() {
    let fixture = Fixture::new();
    let source = fixture.create("source").await;
    source.append_message(user_message("one")).await.unwrap();
    source
        .append_message(assistant_message("two"))
        .await
        .unwrap();

    fixture.inspect(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER fail_fork_entry BEFORE INSERT ON entries
WHEN new.session_id = 'fork' AND new.seq = 2
BEGIN
  SELECT RAISE(ABORT, 'fail fork');
END;",
        )
        .unwrap();
    });

    let error = fixture
        .repo
        .fork(
            &source.get_metadata().await.unwrap(),
            &ForkOptions::tree(),
            &fixture.create_options("fork"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "storage");

    let (sessions, entries): (i64, i64) = fixture.inspect(|conn| {
        (
            conn.query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?",
                ["fork"],
                |row| row.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT COUNT(*) FROM entries WHERE session_id = ?",
                ["fork"],
                |row| row.get(0),
            )
            .unwrap(),
        )
    });
    assert_eq!((sessions, entries), (0, 0));
}

#[tokio::test]
async fn closes_active_sessions_when_the_repository_is_closed() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;

    fixture.repo.close().await.unwrap();

    let error = session
        .append_message(user_message("late"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "storage");
    assert!(
        error
            .message()
            .contains("SQLite session session-1 is closed"),
        "unexpected message: {error}"
    );
}

#[tokio::test]
async fn rejects_a_missing_lane_leaf_when_listing_lanes_and_opening() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let metadata = session.get_metadata().await.unwrap();

    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE lanes SET leaf_id = ? WHERE session_id = ? AND lane = ?",
            params!["missing", metadata.id, "main"],
        )
        .unwrap();
    });

    for error in [
        session.get_lanes().await.unwrap_err(),
        fixture.repo.open(&metadata).await.err().unwrap(),
    ] {
        assert_eq!(error.code(), "storage");
        assert!(
            error
                .message()
                .contains("Lane main points at missing entry missing"),
            "unexpected message: {error}"
        );
    }
}

#[tokio::test]
async fn rejects_stored_session_metadata_that_is_not_a_json_object() {
    for (stored, expected) in [
        ("not json", "metadata is not valid JSON"),
        ("[]", "metadata must be an object"),
    ] {
        let fixture = Fixture::new();
        fixture.create("session-1").await;
        fixture.inspect(|conn| {
            conn.execute(
                "UPDATE sessions SET metadata = ? WHERE id = ?",
                params![stored, "session-1"],
            )
            .unwrap();
        });

        let error = fixture
            .repo
            .list(&Default::default())
            .await
            .expect_err("list must fail");
        assert_eq!(error.code(), "storage");
        assert!(
            error.message().contains(expected),
            "unexpected message: {error}"
        );
    }
}

#[tokio::test]
async fn rejects_stored_session_names_that_are_not_json_strings() {
    for (stored, expected) in [
        ("not json", "name is not valid JSON"),
        ("{}", "name must be a string"),
    ] {
        let fixture = Fixture::new();
        let session = fixture.create("session-1").await;
        session.set_name(Some("valid name")).await.unwrap();
        fixture.inspect(|conn| {
            conn.execute(
                "UPDATE facts SET value = ? WHERE session_id = ? AND kind = 'name'",
                params![stored, "session-1"],
            )
            .unwrap();
        });

        for error in [
            fixture.repo.list(&Default::default()).await.err().unwrap(),
            session.get_metadata().await.unwrap_err(),
        ] {
            assert_eq!(error.code(), "storage");
            assert!(
                error.message().contains(expected),
                "unexpected message: {error}"
            );
        }
    }
}

#[tokio::test]
async fn omits_a_cleared_session_name_from_metadata() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    session.set_name(Some("Temporary")).await.unwrap();
    assert_eq!(
        session.get_metadata().await.unwrap().get_str("name"),
        Some("Temporary")
    );

    session.set_name(None).await.unwrap();

    assert_eq!(session.get_name().await.unwrap(), None);
    assert!(session.get_metadata().await.unwrap().get("name").is_none());
    let listed = fixture.repo.list(&Default::default()).await.unwrap();
    assert!(listed[0].get("name").is_none());
}

#[tokio::test]
async fn fails_loudly_when_a_stored_entry_cannot_be_decoded() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let entry_id = session
        .append_message(user_message("message"))
        .await
        .unwrap();
    let metadata = session.get_metadata().await.unwrap();

    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET payload = ? WHERE session_id = ? AND id = ?",
            params!["not json", metadata.id, entry_id],
        )
        .unwrap();
    });

    let reopened = fixture.repo.open(&metadata).await.unwrap();
    let error = all_entries(&reopened).await.unwrap_err();
    assert_eq!(error.code(), "invalid_entry");
}

#[tokio::test]
async fn fails_loudly_when_a_stored_record_cannot_be_decoded() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
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

    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE records SET payload = ? WHERE session_id = ? AND id = ?",
            params!["not json", "session-1", "record-1"],
        )
        .unwrap();
    });

    let error = session.find_records(&RecordQuery::new()).await.unwrap_err();
    assert_eq!(error.code(), "storage");
    assert!(
        error.message().contains("failed to decode payload"),
        "unexpected message: {error}"
    );
}

#[tokio::test]
async fn does_not_publish_connection_state_when_an_append_transaction_fails() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;

    fixture.inspect(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER fail_branch_tip_insert
             BEFORE INSERT ON branch_tips
             BEGIN
               SELECT RAISE(ABORT, 'branch insert failed');
             END;",
        )
        .unwrap();
    });

    let error = session
        .append_message(user_message("root"))
        .await
        .unwrap_err();
    assert!(
        error.message().contains("branch insert failed"),
        "unexpected message: {error}"
    );

    let (leaf, entries): (Option<String>, i64) = fixture.inspect(|conn| {
        (
            conn.query_row(
                "SELECT leaf_id FROM lanes WHERE session_id = ? AND lane = ?",
                params!["session-1", "main"],
                |row| row.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT COUNT(*) FROM entries WHERE session_id = ?",
                ["session-1"],
                |row| row.get(0),
            )
            .unwrap(),
        )
    });
    assert_eq!((leaf, entries), (None, 0));
    assert_eq!(session.get_stats().await.unwrap().message_count, 0);

    fixture.inspect(|conn| {
        conn.execute_batch("DROP TRIGGER fail_branch_tip_insert")
            .unwrap();
    });
    let entry_id = session.append_message(user_message("root")).await.unwrap();
    assert_eq!(ids(&all_entries(&session).await.unwrap()), vec![entry_id]);
    assert_eq!(session.get_stats().await.unwrap().message_count, 1);
}

#[tokio::test]
async fn accounts_for_assistant_compaction_and_branch_summary_usage() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let user_id = session.append_message(user_message("one")).await.unwrap();

    let assistant_usage = usage(100, 25, 40, 10, cost(0.1, 0.2, 0.03, 0.04, 0.37));
    let assistant_id = session
        .append_message(assistant_message_with_usage("two", assistant_usage.clone()))
        .await
        .unwrap();
    session
        .append_record(&usage_record(
            "assistant-usage",
            UsageCause::Assistant,
            &assistant_id,
            assistant_usage.clone(),
        ))
        .await
        .unwrap();

    let compaction_usage = usage(1, 2, 3, 4, cost(0.01, 0.02, 0.03, 0.04, 0.1));
    let compaction_id = append_compaction(&session, "summary", 200, Some(compaction_usage.clone()))
        .await
        .unwrap();
    session
        .append_record(&usage_record(
            "compaction-usage",
            UsageCause::Compaction,
            &compaction_id,
            compaction_usage,
        ))
        .await
        .unwrap();

    let branch_usage = usage(5, 6, 7, 8, cost(0.05, 0.06, 0.07, 0.08, 0.26));
    let branch_summary_id = move_main_lane(
        &session,
        Some(&user_id),
        Some(("branch summary", Some(branch_usage.clone()))),
    )
    .await
    .unwrap()
    .expect("branch summary");
    session
        .append_record(&usage_record(
            "branch-summary-usage",
            UsageCause::BranchSummary,
            &branch_summary_id,
            branch_usage,
        ))
        .await
        .unwrap();

    let stats = session.get_stats().await.unwrap();
    assert_eq!(
        SessionStats {
            cost_total: (stats.cost_total * 100.0).round() / 100.0,
            ..stats
        },
        SessionStats {
            message_count: 2,
            cached_tokens: 50,
            uncached_tokens: 128,
            total_tokens: 211,
            cost_total: 0.73,
        }
    );
}

fn usage_record(id: &str, cause: UsageCause, entry_id: &str, usage: pi_core::Usage) -> NewRecord {
    NewRecord::new(
        id,
        "main",
        RecordPayload::Usage(UsageRecord {
            cause,
            run_id: Some("run".into()),
            entry_id: Some(entry_id.into()),
            tool_call_id: None,
            attempt: Some(1),
            stop_reason: Some(pi_core::StopReason::Stop),
            details: None,
            usage,
        }),
    )
}

// ---------------------------------------------------------------------------
// log-query.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn does_not_decode_rows_beyond_the_requested_log_limit() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let root_id = session.append_message(user_message("root")).await.unwrap();
    session.set_name(Some("name")).await.unwrap();
    let tail_id = session.append_message(user_message("tail")).await.unwrap();

    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET payload = ? WHERE session_id = ? AND id = ?",
            params!["not json", "session-1", tail_id],
        )
        .unwrap();
    });

    let first = session
        .get_log(&LogOptions {
            after_seq: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    match &first[..] {
        [LogItem::Entry { seq: 1, entry }] => assert_eq!(entry.id, root_id),
        other => panic!("unexpected log window: {other:?}"),
    }

    let second = session
        .get_log(&LogOptions {
            after_seq: Some(1),
            limit: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(
        second,
        vec![LogItem::Name {
            seq: 2,
            name: Some("name".into())
        }]
    );
}
