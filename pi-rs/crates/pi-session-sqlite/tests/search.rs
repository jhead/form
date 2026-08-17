//! Port of `test/search.test.ts`.

mod common;

use common::*;
use pi_session::repo::{SearchBackend, SessionSearchOptions};
use pi_session::{EntryType, SessionCreateOptions};
use pi_session_sqlite::SqliteSearchBackend;
use rusqlite::params;
use serde_json::json;

fn search_backend(fixture: &Fixture) -> SqliteSearchBackend {
    SqliteSearchBackend::new(fixture.database_path())
}

async fn hits(backend: &SqliteSearchBackend, text: &str) -> Vec<(String, String)> {
    backend
        .search(text, &SessionSearchOptions::default())
        .await
        .unwrap()
        .into_iter()
        .map(|hit| (hit.session_id, hit.entry_id))
        .collect()
}

#[tokio::test]
async fn matches_trigrams() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    let mut included_options = fixture.create_options("included");
    included_options.metadata = Some(
        json!({ "name": "application-owned" })
            .as_object()
            .cloned()
            .unwrap(),
    );
    let included = fixture.repo.create(&included_options).await.unwrap();
    let excluded = fixture
        .repo
        .create(
            &SessionCreateOptions::new()
                .with_id("excluded")
                .with_cwd(format!("{}/other", fixture.cwd())),
        )
        .await
        .unwrap();

    let entry_id = included
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();
    included.set_name(Some("Canonical name")).await.unwrap();
    let excluded_entry_id = excluded
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();

    let mut auth = hits(&backend, "auth").await;
    auth.sort();
    assert_eq!(
        auth,
        vec![
            ("excluded".to_string(), excluded_entry_id.clone()),
            ("included".to_string(), entry_id.clone()),
        ]
    );
    // A trigram tokenizer matches substrings, not just whole words.
    let mut partial = hits(&backend, "uth").await;
    partial.sort();
    assert_eq!(
        partial,
        vec![
            ("excluded".to_string(), excluded_entry_id),
            ("included".to_string(), entry_id),
        ]
    );

    let full = backend
        .search("auth", &SessionSearchOptions::default())
        .await
        .unwrap();
    assert!(full.iter().all(|hit| hit.timestamp.is_some()));
    assert!(full.iter().all(|hit| hit.score.is_some()));
}

#[tokio::test]
async fn handles_quoted_search_text_without_exposing_fts_syntax() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    assert!(hits(&backend, "missing \"phrase\"").await.is_empty());
}

#[tokio::test]
async fn rebuilds_existing_entries_when_fts_is_first_initialized() {
    let fixture = Fixture::new();
    let session = fixture.create("session-1").await;
    let entry_id = session
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();

    // The index is created lazily, after the entry already exists.
    let backend = search_backend(&fixture);
    assert_eq!(
        hits(&backend, "auth").await,
        vec![("session-1".to_string(), entry_id)]
    );
}

#[tokio::test]
async fn honors_entry_type_filters() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    let session = fixture.create("session-1").await;
    let message_entry_id = session
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();
    session
        .append_custom_entry(
            "note",
            Some(json!({ "text": "Find the auth custom entry" })),
        )
        .await
        .unwrap();

    let filtered = backend
        .search(
            "auth",
            &SessionSearchOptions {
                entry_types: Some(vec![EntryType::Message]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        filtered
            .into_iter()
            .map(|hit| (hit.session_id, hit.entry_id))
            .collect::<Vec<_>>(),
        vec![("session-1".to_string(), message_entry_id)]
    );

    // An explicitly empty filter matches nothing.
    assert!(backend
        .search(
            "auth",
            &SessionSearchOptions {
                entry_types: Some(vec![]),
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn honors_result_limits() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    let first = fixture.create("session-1").await;
    let second = fixture.create("session-2").await;
    first
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();
    second
        .append_message(user_message("Find the auth defect too"))
        .await
        .unwrap();

    let limited = backend
        .search(
            "auth",
            &SessionSearchOptions {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert!(backend
        .search(
            "auth",
            &SessionSearchOptions {
                limit: Some(0),
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn removes_deleted_session_entries_from_the_index() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    let session = fixture.create("session-1").await;
    session
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();
    assert_eq!(hits(&backend, "auth").await.len(), 1);

    fixture
        .repo
        .delete(&session.get_metadata().await.unwrap())
        .await
        .unwrap();

    assert!(hits(&backend, "auth").await.is_empty());
}

#[tokio::test]
async fn indexes_and_removes_entries_through_triggers_after_initialization() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    // Initialize the index before any session exists.
    assert!(hits(&backend, "auth").await.is_empty());

    let session = fixture.create("session-1").await;
    session
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();
    assert_eq!(hits(&backend, "auth").await.len(), 1);

    fixture
        .repo
        .delete(&session.get_metadata().await.unwrap())
        .await
        .unwrap();
    assert!(hits(&backend, "auth").await.is_empty());
}

#[tokio::test]
async fn removes_directly_deleted_entries_through_triggers() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    let session = fixture.create("session-1").await;
    let entry_id = session
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();
    assert_eq!(hits(&backend, "auth").await.len(), 1);

    fixture.inspect(|conn| {
        conn.execute(
            "DELETE FROM entries WHERE session_id = ? AND id = ?",
            params!["session-1", entry_id],
        )
        .unwrap();
    });

    assert!(hits(&backend, "auth").await.is_empty());
}

#[tokio::test]
async fn does_not_initialize_fts_for_canonical_writes_or_blank_searches() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    assert!(hits(&backend, "  ").await.is_empty());
    let session = fixture.create("session-1").await;

    let fts_exists: bool = fixture.inspect(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_search_fts'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    });
    assert!(!fts_exists, "a blank search must not create the index");
    session
        .append_message(user_message("still writable"))
        .await
        .unwrap();
}

#[tokio::test]
async fn rolls_back_canonical_appends_when_fts_trigger_writes_fail() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    hits(&backend, "initialize").await;
    let session = fixture.create("session-1").await;
    // Close the search connection so the DROP is not blocked by its lock.
    backend.close().await;

    fixture.inspect(|conn| {
        conn.execute_batch("DROP TABLE session_search_fts").unwrap();
    });

    // The triggers survive the table, so the append fails and rolls back
    // rather than silently desyncing the index.
    assert!(session
        .append_message(user_message("must roll back"))
        .await
        .is_err());
    assert!(all_entries(&session).await.unwrap().is_empty());
}

#[tokio::test]
async fn rolls_back_canonical_deletion_when_fts_cleanup_fails() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    hits(&backend, "initialize").await;
    let session = fixture.create("session-1").await;
    session
        .append_message(user_message("must remain"))
        .await
        .unwrap();
    let metadata = session.get_metadata().await.unwrap();
    backend.close().await;

    fixture.inspect(|conn| {
        conn.execute_batch("DROP TABLE session_search_fts").unwrap();
    });

    assert!(fixture.repo.delete(&metadata).await.is_err());
    let reopened = fixture.repo.open(&metadata).await.unwrap();
    assert_eq!(all_entries(&reopened).await.unwrap().len(), 1);
}

#[tokio::test]
async fn initializes_canonical_storage_when_searched_before_the_first_session() {
    let fixture = Fixture::new();
    let backend = search_backend(&fixture);
    assert!(hits(&backend, "auth").await.is_empty());

    let session = fixture.create("session-1").await;
    let entry_id = session
        .append_message(user_message("Find the auth defect"))
        .await
        .unwrap();

    assert_eq!(
        hits(&backend, "auth").await,
        vec![("session-1".to_string(), entry_id)]
    );
    session
        .append_message(user_message("Still writable"))
        .await
        .unwrap();
}

#[tokio::test]
async fn reports_a_storage_error_when_the_database_cannot_be_opened() {
    let root = tempfile::tempdir().unwrap();
    // A directory where the database file should be: opening must fail loudly.
    let path = root.path().join("sessions.sqlite");
    std::fs::create_dir(&path).unwrap();
    let backend = SqliteSearchBackend::new(path);

    let error = backend
        .search("auth", &SessionSearchOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "storage");
}
