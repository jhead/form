//! Port of `test/writer-leases.test.ts`.
//!
//! The lease is what makes two processes sharing one database file safe. Each
//! `SqliteSessionRepo` over the same path stands in for upstream's separate
//! repositories.

mod common;

use std::time::Duration;

use common::*;
use pi_session::{EntryQuery, SessionCreateOptions, SessionListOptions};
use pi_session_sqlite::{SqliteSessionRepo, SqliteWriterLeaseOptions};
use rusqlite::params;
use serde_json::json;

fn lease(ttl_ms: i64, heartbeat_interval_ms: i64) -> SqliteWriterLeaseOptions {
    SqliteWriterLeaseOptions {
        ttl_ms: Some(ttl_ms),
        heartbeat_interval_ms: Some(heartbeat_interval_ms),
    }
}

#[tokio::test]
async fn shares_one_write_queue_across_repeated_opens_in_one_repository() {
    let fixture = Fixture::new();
    let session = fixture.create("session").await;
    let reopened = fixture
        .repo
        .open(&session.get_metadata().await.unwrap())
        .await
        .unwrap();

    let (first, second) = tokio::join!(
        session.append_message(user_message("first")),
        reopened.append_message(user_message("second"))
    );
    let (first, second) = (first.unwrap(), second.unwrap());

    assert_eq!(
        ids(&all_entries(&session).await.unwrap()),
        vec![first, second]
    );
}

#[test]
fn rejects_invalid_lease_timing() {
    for (options, message) in [
        (lease(0, 1), "writerLease.ttlMs must be positive"),
        (
            lease(100, 100),
            "writerLease.heartbeatIntervalMs must be positive and less than ttlMs",
        ),
    ] {
        let error = SqliteSessionRepo::with_writer_lease("/tmp/pi-unused.sqlite", options)
            .expect_err("invalid lease timing must be rejected");
        assert_eq!(error.code(), "invalid_payload");
        assert_eq!(error.message(), message);
    }
}

#[tokio::test]
async fn lists_complete_metadata_without_acquiring_active_writer_leases() {
    let fixture = Fixture::new();
    let reader = fixture.peer_repo(SqliteWriterLeaseOptions::default());

    let mut first_options = fixture.create_options("session-1");
    first_options.metadata = Some(
        json!({ "profile": "reviewer" })
            .as_object()
            .cloned()
            .unwrap(),
    );
    let first = fixture.repo.create(&first_options).await.unwrap();

    let mut second_options = fixture.create_options("session-2");
    second_options.parent_session_id = Some("session-1".into());
    second_options.metadata = Some(
        json!({ "profile": "writer", "name": "application-owned name" })
            .as_object()
            .cloned()
            .unwrap(),
    );
    let second = fixture.repo.create(&second_options).await.unwrap();

    first.set_name(Some("Review session")).await.unwrap();
    second.set_name(Some("Write session")).await.unwrap();
    let first_metadata = first.get_metadata().await.unwrap();
    let second_metadata = second.get_metadata().await.unwrap();
    assert_eq!(first_metadata.get_str("name"), Some("Review session"));
    assert_eq!(
        second_metadata.get("metadata"),
        Some(&json!({ "profile": "writer", "name": "application-owned name" }))
    );

    let leases_before = read_leases(&fixture);
    let mut listed = reader
        .list(&SessionListOptions {
            cwd: Some(fixture.cwd()),
        })
        .await
        .unwrap();
    listed.sort_by(|left, right| left.id.cmp(&right.id));
    let mut expected = vec![first_metadata.clone(), second_metadata];
    expected.sort_by(|left, right| left.id.cmp(&right.id));
    assert_eq!(listed, expected);
    assert_eq!(
        read_leases(&fixture),
        leases_before,
        "list must not touch leases"
    );

    let error = reader.open(&first_metadata).await.err().unwrap();
    assert_eq!(error.code(), "storage");
    assert!(
        error.message().contains("already has an active writer"),
        "unexpected message: {error}"
    );
}

#[tokio::test]
async fn rejects_a_second_writer_until_the_first_releases_its_claim() {
    let fixture = Fixture::new();
    let second_repo = fixture.peer_repo(SqliteWriterLeaseOptions::default());
    let first = fixture.create("session-1").await;
    let metadata = first.get_metadata().await.unwrap();

    let error = second_repo.open(&metadata).await.err().unwrap();
    assert_eq!(error.code(), "storage");
    assert!(
        error.message().contains("already has an active writer"),
        "unexpected message: {error}"
    );

    fixture.repo.close().await.unwrap();

    let second = second_repo.open(&metadata).await.unwrap();
    second
        .append_message(user_message("new owner"))
        .await
        .unwrap();
}

#[tokio::test]
async fn fences_a_stale_owner_after_an_expired_lease_is_taken_over() {
    let timings = lease(120_000, 60_000);
    let fixture = Fixture::with_lease(timings);
    let second_repo = fixture.peer_repo(timings);
    let first = fixture.create("session-1").await;
    let metadata = first.get_metadata().await.unwrap();

    // Simulate a writer that stalled long enough for its lease to lapse.
    fixture.inspect(|conn| {
        conn.execute(
            "UPDATE writer_leases SET expires_at_ms = 0 WHERE session_id = ?",
            [&metadata.id],
        )
        .unwrap();
    });

    let second = second_repo.open(&metadata).await.unwrap();
    let error = first
        .append_message(user_message("stale owner"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "storage");
    assert!(
        error.message().contains("writer lease was lost"),
        "unexpected message: {error}"
    );
    assert!(second
        .find_entries(&EntryQuery::new())
        .await
        .unwrap()
        .is_empty());

    let current = read_leases(&fixture);
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].2, 2, "takeover must bump the fence");

    // The stale owner closing must not delete the new owner's lease.
    fixture.repo.close().await.unwrap();
    assert_eq!(read_leases(&fixture), current);
    second
        .append_message(user_message("current owner"))
        .await
        .unwrap();
}

#[tokio::test]
async fn serializes_lease_checked_writes_for_sessions_sharing_one_connection() {
    let fixture = Fixture::new();
    let first = fixture.create("session-1").await;
    let second = fixture.create("session-2").await;

    let (one, two) = tokio::join!(
        first.append_message(user_message("first")),
        second.append_message(user_message("second"))
    );
    one.unwrap();
    two.unwrap();
}

/// Upstream drives this with fake timers; the port uses a short real interval
/// instead, because the heartbeat runs on a `tokio::spawn`ed task and a paused
/// clock cannot see the blocking renew it performs.
#[tokio::test]
async fn renews_an_idle_writer_lease_with_a_heartbeat() {
    let fixture = Fixture::with_lease(lease(2_000, 100));
    let session = fixture.create("session-1").await;
    let metadata = session.get_metadata().await.unwrap();

    let read_expiry = || {
        fixture.inspect(|conn| {
            conn.query_row(
                "SELECT expires_at_ms FROM writer_leases WHERE session_id = ?",
                [&metadata.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        })
    };
    let initial = read_expiry();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        read_expiry() > initial,
        "an idle session must keep its lease alive"
    );
}

#[tokio::test]
async fn a_default_repository_uses_upstreams_lease_timings() {
    let root = tempfile::tempdir().unwrap();
    let repo = SqliteSessionRepo::new(root.path().join("sessions.sqlite"));
    let created_at = pi_core::now_ms();
    let session = repo
        .create(
            &SessionCreateOptions::new()
                .with_id("session-1")
                .with_cwd(root.path().display().to_string()),
        )
        .await
        .unwrap();
    let metadata = session.get_metadata().await.unwrap();

    let expires: i64 = inspect_at(&root.path().join("sessions.sqlite"), |conn| {
        conn.query_row(
            "SELECT expires_at_ms FROM writer_leases WHERE session_id = ?",
            [&metadata.id],
            |row| row.get(0),
        )
        .unwrap()
    });
    // The default TTL is 30s; allow slack for the clock read either side.
    assert!(
        (expires - created_at - 30_000).abs() < 2_000,
        "unexpected default lease expiry: {expires} vs {created_at}"
    );
}

fn read_leases(fixture: &Fixture) -> Vec<(String, String, i64, i64)> {
    fixture.inspect(|conn| {
        conn.prepare(
            "SELECT session_id, owner_id, fence, expires_at_ms FROM writer_leases ORDER BY session_id",
        )
        .unwrap()
        .query_map(params![], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    })
}
