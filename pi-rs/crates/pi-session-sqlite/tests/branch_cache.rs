//! Port of `test/branch-cache.test.ts` and `test/branch-query.test.ts`.
//!
//! These cases are about the *derived* cache: the invariants that let bounded
//! branch reads skip decoding, and the deliberate refusal to repair a damaged
//! cache implicitly.

mod common;

use common::*;
use pi_session::{BranchQuery, EntryOrder, EntryType};
use rusqlite::params;

#[tokio::test]
async fn collects_complete_root_paths_for_branches_created_after_compaction() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let root = session.append_message(user_message("root")).await.unwrap();
    let kept = session.append_message(user_message("kept")).await.unwrap();
    let compaction = append_compaction(&session, "summary", 100, None)
        .await
        .unwrap();
    session
        .append_message(assistant_message("first child"))
        .await
        .unwrap();
    move_main_lane(&session, Some(&compaction), None)
        .await
        .unwrap();
    let branched = session
        .append_message(assistant_message("branched child"))
        .await
        .unwrap();

    let entries = fixture.inspect(|conn| {
        let branch_id: String = conn
            .query_row(
                "SELECT branch_id FROM branch_entries WHERE session_id = ? AND entry_id = ?",
                params!["session-1", branched],
                |row| row.get(0),
            )
            .expect("branched entry cache row");
        conn.prepare(
            "SELECT entry_id FROM branch_entries WHERE session_id = ? AND branch_id = ? ORDER BY entry_seq",
        )
        .unwrap()
        .query_map(params!["session-1", branch_id], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    });
    assert_eq!(entries, vec![root, kept, compaction, branched]);
}

#[tokio::test]
async fn reads_only_the_compacted_branch_window_from_the_complete_cache() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let old = session.append_message(user_message("old")).await.unwrap();
    session.append_message(user_message("kept")).await.unwrap();
    let compaction = append_compaction(&session, "summary", 100, None)
        .await
        .unwrap();
    let leaf = session
        .append_message(assistant_message("new"))
        .await
        .unwrap();

    // Corrupt an entry *outside* the window; the read must not touch it.
    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET payload = ? WHERE session_id = ? AND id = ?",
            params!["not json", "session-1", old],
        )
        .unwrap();
    });

    assert_eq!(
        ids(&branch_window(&session).await.unwrap()),
        vec![compaction, leaf]
    );
}

#[tokio::test]
async fn preserves_nested_compaction_boundaries_when_reading_the_cache() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    session.append_message(user_message("root")).await.unwrap();
    append_compaction(&session, "first summary", 100, None)
        .await
        .unwrap();
    session
        .append_message(user_message("middle"))
        .await
        .unwrap();
    let second_compaction = append_compaction(&session, "second summary", 200, None)
        .await
        .unwrap();
    let leaf = session
        .append_message(assistant_message("new"))
        .await
        .unwrap();

    assert_eq!(
        ids(&branch_window(&session).await.unwrap()),
        vec![second_compaction, leaf]
    );
}

#[tokio::test]
async fn rejects_reads_and_writes_without_repairing_a_missing_cache() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    session.append_message(user_message("root")).await.unwrap();
    session
        .append_message(assistant_message("child"))
        .await
        .unwrap();

    fixture.inspect(|conn| {
        conn.execute(
            "DELETE FROM branch_tips WHERE session_id = ?",
            ["session-1"],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM branch_entries WHERE session_id = ?",
            ["session-1"],
        )
        .unwrap();
    });

    let read = branch_window(&session).await.unwrap_err();
    assert_eq!(read.code(), "invalid_entry");
    let write = session
        .append_message(assistant_message("later"))
        .await
        .unwrap_err();
    assert_eq!(write.code(), "invalid_entry");
    assert!(
        write
            .message()
            .contains("has no branch containing parent entry"),
        "unexpected message: {write}"
    );

    // The failed append must not have left a partial cache behind.
    let remaining: i64 = fixture.inspect(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM branch_entries WHERE session_id = ?",
            ["session-1"],
            |row| row.get(0),
        )
        .unwrap()
    });
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn repairs_the_private_branch_cache_explicitly() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let root = session.append_message(user_message("root")).await.unwrap();
    let child = session
        .append_message(assistant_message("child"))
        .await
        .unwrap();
    let metadata = session.get_metadata().await.unwrap();

    fixture.inspect(|conn| {
        conn.execute(
            "DELETE FROM branch_tips WHERE session_id = ?",
            ["session-1"],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM branch_entries WHERE session_id = ?",
            ["session-1"],
        )
        .unwrap();
    });
    assert_eq!(
        branch_window(&session).await.unwrap_err().code(),
        "invalid_entry"
    );

    fixture.repo.repair_branch_cache(&metadata).await.unwrap();

    // Repair released the session's lease, so re-open before reading.
    let reopened = fixture.repo.open(&metadata).await.unwrap();
    assert_eq!(
        ids(&branch_window(&reopened).await.unwrap()),
        vec![root, child]
    );
}

#[tokio::test]
async fn fails_when_forking_from_a_source_with_a_missing_branch_cache() {
    let fixture = Fixture::new();
    let source = fixture.create("source").await;
    let root = source.append_message(user_message("root")).await.unwrap();
    let child = source
        .append_message(assistant_message("child"))
        .await
        .unwrap();
    assert_ne!(root, child);

    fixture.inspect(|conn| {
        conn.execute("DELETE FROM branch_tips WHERE session_id = ?", ["source"])
            .unwrap();
        conn.execute(
            "DELETE FROM branch_entries WHERE session_id = ?",
            ["source"],
        )
        .unwrap();
    });

    let error = fixture
        .repo
        .fork(
            &source.get_metadata().await.unwrap(),
            &pi_session::ForkOptions::branch_at(&child),
            &fixture.create_options("fork"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_fork_target");
}

#[tokio::test]
async fn fails_when_the_private_branch_cache_is_stale() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let root = session.append_message(user_message("root")).await.unwrap();
    let stale = session
        .append_message(assistant_message("stale"))
        .await
        .unwrap();
    let leaf = session.append_message(user_message("leaf")).await.unwrap();
    assert_ne!(stale, leaf);

    // Re-parent the leaf behind the cache's back.
    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET parent_id = ? WHERE session_id = ? AND id = ?",
            params![root, "session-1", leaf],
        )
        .unwrap();
    });

    let error = session
        .find_entries_on_branch(
            &BranchQuery::new()
                .with_start(&leaf)
                .with_order(EntryOrder::OldestFirst),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_entry");
}

#[tokio::test]
async fn deletes_branch_entries_and_tips_with_the_session() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    session.append_message(user_message("root")).await.unwrap();
    let metadata = session.get_metadata().await.unwrap();

    fixture.repo.delete(&metadata).await.unwrap();

    let (entries, tips): (i64, i64) = fixture.inspect(|conn| {
        (
            conn.query_row(
                "SELECT COUNT(*) FROM branch_entries WHERE session_id = ?",
                ["session-1"],
                |row| row.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT COUNT(*) FROM branch_tips WHERE session_id = ?",
                ["session-1"],
                |row| row.get(0),
            )
            .unwrap(),
        )
    });
    assert_eq!((entries, tips), (0, 0));
}

// ---------------------------------------------------------------------------
// branch-query.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn does_not_decode_entries_outside_bounded_branch_queries() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let root = session.append_message(user_message("root")).await.unwrap();
    let middle = session
        .append_message(assistant_message("middle"))
        .await
        .unwrap();
    let leaf = session.append_message(user_message("leaf")).await.unwrap();

    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET payload = ? WHERE session_id = ? AND id = ?",
            params!["not json", "session-1", middle],
        )
        .unwrap();
        let branch_id: String = conn
            .query_row(
                "SELECT branch_id FROM branch_entries WHERE session_id = ? AND entry_id = ?",
                params!["session-1", leaf],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "DELETE FROM branch_entries WHERE session_id = ? AND branch_id = ? AND entry_id = ?",
            params!["session-1", branch_id, middle],
        )
        .unwrap();
    });

    assert_eq!(
        ids(&session
            .find_entries_on_branch(&BranchQuery::new().with_start(&leaf).with_stop_at_id(&leaf))
            .await
            .unwrap()),
        vec![leaf.clone()]
    );
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start(&leaf)
                    .with_stop_at_id(&root)
                    .with_order(EntryOrder::OldestFirst)
                    .with_limit(1)
            )
            .await
            .unwrap()),
        vec![root]
    );
    let error = session
        .find_entries_on_branch(&BranchQuery::new().with_start(&leaf).with_limit(2))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_entry");
    assert!(
        error
            .message()
            .contains(&format!("Entry {middle} not found")),
        "unexpected message: {error}"
    );
}

#[tokio::test]
async fn does_not_decode_entries_excluded_by_branch_filters_and_limits() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    session.append_message(user_message("root")).await.unwrap();
    let custom = session
        .append_custom_entry("note", Some(serde_json::json!({ "value": 1 })))
        .await
        .unwrap();
    let leaf = session
        .append_message(assistant_message("leaf"))
        .await
        .unwrap();

    // A payload that parses but no longer matches the custom schema.
    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET payload = ? WHERE session_id = ? AND id = ?",
            params!["{}", "session-1", custom],
        )
        .unwrap();
    });
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start(&leaf)
                    .with_type(EntryType::Message)
                    .with_limit(1)
            )
            .await
            .unwrap()),
        vec![leaf.clone()]
    );

    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET payload = ? WHERE session_id = ? AND id = ?",
            params!["not json", "session-1", custom],
        )
        .unwrap();
    });
    assert!(session
        .find_entries_on_branch(
            &BranchQuery::new()
                .with_start(&leaf)
                .with_custom_type("other")
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn does_not_validate_ancestors_beyond_newest_first_stop_bounds() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let root = session.append_message(user_message("root")).await.unwrap();
    let child = session
        .append_message(assistant_message("child"))
        .await
        .unwrap();

    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET parent_id = ? WHERE session_id = ? AND id = ?",
            params!["missing-parent", "session-1", child],
        )
        .unwrap();
    });
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start(&child)
                    .with_stop_at_id(&child)
            )
            .await
            .unwrap()),
        vec![child.clone()]
    );
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start(&child)
                    .with_stop_at_type(EntryType::Message)
            )
            .await
            .unwrap()),
        vec![child.clone()]
    );
    let error = session
        .find_entries_on_branch(&BranchQuery::new().with_start(&child))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_entry");
    assert!(
        error.message().contains("Entry missing-parent not found"),
        "unexpected message: {error}"
    );

    // Now make the two entries point at each other.
    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE entries SET parent_id = ? WHERE session_id = ? AND id = ?",
            params![root, "session-1", child],
        )
        .unwrap();
        conn.execute(
            "UPDATE entries SET parent_id = ? WHERE session_id = ? AND id = ?",
            params![child, "session-1", root],
        )
        .unwrap();
    });
    assert_eq!(
        ids(&session
            .find_entries_on_branch(
                &BranchQuery::new()
                    .with_start(&child)
                    .with_stop_at_id(&child)
            )
            .await
            .unwrap()),
        vec![child.clone()]
    );
    let error = session
        .find_entries_on_branch(&BranchQuery::new().with_start(&child))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_entry");
    assert!(
        error
            .message()
            .contains(&format!("Entry {child} not found")),
        "unexpected message: {error}"
    );
}
