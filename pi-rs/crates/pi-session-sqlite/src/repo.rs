//! The SQLite session repository and storage. Port of `sqlite/repo.ts`.
//!
//! # Sync driver behind an async trait
//!
//! `rusqlite` is synchronous and a `Connection` is `Send` but not `Sync`, so
//! the repository owns exactly one connection behind a [`tokio::sync::Mutex`]
//! and every trait method takes that lock, then hands the guard to
//! [`tokio::task::spawn_blocking`]. That is not a compromise: upstream is also
//! single-writer — its `SerialOperationQueue` serializes work over one
//! `DatabaseSync` — and tokio's mutex is FIFO-fair, so the queue's ordering
//! guarantee survives the port. Concurrent `append` calls therefore commit in
//! the order they were first polled, which is what the conformance suite's
//! linearization case asserts.
//!
//! # Writer leases
//!
//! Cross-process safety comes from the `writer_leases` table, not from the
//! mutex. Opening a session takes a fenced lease; every write renews it inside
//! the same transaction as the write itself, so a stale owner whose lease was
//! taken over fails loudly instead of corrupting the session. An idle session
//! keeps its lease alive with a background heartbeat task.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex as SyncMutex;
use rusqlite::Connection;
use serde_json::{Map, Value};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use pi_core::now_ms;
use pi_session::repo::{BranchStore, EntryStore, SessionRepo, SessionStorage};
use pi_session::{
    BoundBranchQuery, Entry, EntryOrder, EntryQuery, EntryType, ForkOptions, ForkPosition,
    ForkScope, LanePointer, LaneRecord, LogItem, LogOptions, NewRecord, ProvisionedEntry,
    RecordPayload, RecordQuery, Session, SessionCreateOptions, SessionError, SessionListOptions,
    SessionMetadata, SessionResult, SessionStats,
};

use crate::branch_cache::{
    append_entry_to_branch_cache, build_cached_branch, delete_branch_cache, rebuild_branch_cache,
};
use crate::migrations::apply_migrations;
use crate::sql::{transaction, SqlQuery};
use crate::storage::branch_entries::{
    query_cached_branch_rows, read_cached_branch, CachedBranchEntryRow, CachedBranchQuery,
};
use crate::storage::branch_tips::read_branch_tip_ids;
use crate::storage::entries::{
    decode_entry, decode_entry_parts, delete_entry_rows, entry_payload, id_exists_in_entries,
    insert_entry_row, read_entry_row, read_entry_rows, EntryRow, EntryRowQuery, NewEntryRow,
};
use crate::storage::facts::{
    append_fact, decode_fact_string, delete_fact_rows, read_fact_rows, read_latest_fact,
    read_latest_label_facts, FactRow,
};
use crate::storage::lanes::{
    create_initial_lane, create_lane as insert_lane, delete_lane_rows, finish_lane_operation,
    move_lane as update_lane, read_lane, read_lane_head, read_lane_move_rows, read_lanes,
    set_lane_leaf, start_lane_operation, LaneMoveRow,
};
use crate::storage::records::{
    append_record_row, decode_record, decode_record_row, delete_record_rows, id_exists_in_records,
    operation_kind_column, read_open_operation_rows, read_record_rows, record_op_kind,
    record_run_id, NewRecordRow, RecordRow, RecordRowQuery,
};
use crate::storage::session_sequences::{
    advance_sequence, create_sequence, delete_sequence, get_next_sequence, set_next_sequence,
};
use crate::storage::session_stats::{
    add_usage_to_stats, create_stats, delete_stats, increment_message_count, read_stats,
};
use crate::storage::sessions::{
    decode_session_metadata, delete_session_row, insert_session_row, read_session_row,
    read_session_rows, session_exists, NewSessionRow, SessionRow,
};
use crate::storage::writer_leases::{
    acquire_writer_lease, delete_writer_lease, release_writer_lease, renew_writer_lease,
    WriterLease,
};

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Writer-lease timing. Port of `SqliteWriterLeaseOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SqliteWriterLeaseOptions {
    /// Time without a successful heartbeat before another writer may take over.
    /// Default: 30 seconds.
    pub ttl_ms: Option<i64>,
    /// Idle heartbeat cadence. Default: 10 seconds. Must be less than `ttl_ms`.
    pub heartbeat_interval_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedWriterLeaseOptions {
    ttl_ms: i64,
    heartbeat_interval_ms: i64,
}

fn resolve_writer_lease_options(
    options: SqliteWriterLeaseOptions,
) -> SessionResult<ResolvedWriterLeaseOptions> {
    let ttl_ms = options.ttl_ms.unwrap_or(30_000);
    let heartbeat_interval_ms = options.heartbeat_interval_ms.unwrap_or(10_000);
    if ttl_ms <= 0 {
        return Err(SessionError::invalid_payload(
            "writerLease.ttlMs must be positive",
        ));
    }
    if heartbeat_interval_ms <= 0 || heartbeat_interval_ms >= ttl_ms {
        return Err(SessionError::invalid_payload(
            "writerLease.heartbeatIntervalMs must be positive and less than ttlMs",
        ));
    }
    Ok(ResolvedWriterLeaseOptions {
        ttl_ms,
        heartbeat_interval_ms,
    })
}

fn active_writer_error(session_id: &str) -> SessionError {
    SessionError::storage(format!(
        "SQLite session {session_id} already has an active writer"
    ))
}

fn lost_writer_error(session_id: &str) -> SessionError {
    SessionError::storage(format!("SQLite session {session_id} writer lease was lost"))
}

// ---------------------------------------------------------------------------
// Connection plumbing
// ---------------------------------------------------------------------------

type SharedConnection = Arc<AsyncMutex<Connection>>;

fn join_error(error: tokio::task::JoinError) -> SessionError {
    SessionError::storage(format!("SQLite worker task failed: {error}"))
}

/// Runs `body` on a blocking worker holding the connection lock.
async fn with_connection<T, F>(db: &SharedConnection, body: F) -> SessionResult<T>
where
    F: FnOnce(&Connection) -> SessionResult<T> + Send + 'static,
    T: Send + 'static,
{
    let guard: OwnedMutexGuard<Connection> = db.clone().lock_owned().await;
    tokio::task::spawn_blocking(move || body(&guard))
        .await
        .map_err(join_error)?
}

/// `configureSqliteDatabase` — WAL, durable commits, and a busy timeout so a
/// second process waits instead of failing immediately.
fn configure_connection(conn: &Connection) -> SessionResult<()> {
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
        .map_err(|error| SessionError::storage(format!("Failed to enable WAL: {error}")))?;
    SqlQuery::raw("PRAGMA synchronous=FULL").exec(conn)?;
    conn.busy_timeout(Duration::from_millis(5_000))
        .map_err(|error| SessionError::storage(format!("Failed to set busy_timeout: {error}")))?;
    Ok(())
}

pub(crate) fn open_configured_connection(path: &Path) -> SessionResult<Connection> {
    let conn = Connection::open(path).map_err(|error| {
        SessionError::storage(format!(
            "Failed to open SQLite database {}: {error}",
            path.display()
        ))
    })?;
    match configure_connection(&conn).and_then(|()| apply_migrations(&conn)) {
        Ok(()) => Ok(conn),
        Err(error) => {
            let _ = conn.close();
            Err(error)
        }
    }
}

pub(crate) async fn create_parent_directory(path: &Path) -> SessionResult<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    tokio::fs::create_dir_all(parent).await.map_err(|error| {
        SessionError::storage(format!(
            "Failed to create SQLite sessions directory {}: {error}",
            parent.display()
        ))
    })
}

pub(crate) fn absolute_path(path: &Path) -> SessionResult<PathBuf> {
    std::path::absolute(path).map_err(|error| {
        SessionError::storage(format!(
            "Failed to resolve SQLite sessions database {}: {error}",
            path.display()
        ))
    })
}

// ---------------------------------------------------------------------------
// Decoding helpers
// ---------------------------------------------------------------------------

fn entry_row_from_cached(row: &CachedBranchEntryRow) -> SessionResult<Entry> {
    decode_entry_parts(
        &row.id,
        row.entry_seq,
        row.parent_id.as_deref(),
        &row.entry_type,
        row.timestamp,
        &row.payload,
    )
}

/// Port of `validateCachedBranchRows`.
///
/// The cache is derived, so a scan that returns a *complete* window must also
/// prove that window is a contiguous parent chain. Filtered scans skip the
/// check because their gaps are intentional.
fn validate_cached_branch_rows(
    rows: &[CachedBranchEntryRow],
    query: &BoundBranchQuery,
) -> SessionResult<()> {
    if rows.is_empty() || query.query.entry_type.is_some() || query.query.custom_type.is_some() {
        return Ok(());
    }
    let mut path: Vec<&CachedBranchEntryRow> = rows.iter().collect();
    path.sort_by_key(|row| row.entry_seq);
    let should_include_root = query.bounds.stop_at_id.is_none()
        && query.bounds.stop_at_type.is_none()
        && query.query.cursor.is_none()
        && (query.query.order_or_default().is_oldest_first() || query.query.limit.is_none());
    let missing = |parent: Option<&str>| {
        SessionError::invalid_entry(format!("Entry {} not found", parent.unwrap_or("null")))
    };
    if should_include_root && path[0].parent_id.is_some() {
        return Err(missing(path[0].parent_id.as_deref()));
    }
    for window in path.windows(2) {
        let (previous, current) = (window[0], window[1]);
        if current.parent_id.as_deref() != Some(previous.id.as_str()) {
            return Err(missing(current.parent_id.as_deref()));
        }
    }
    Ok(())
}

/// Port of `matchesEntryQuery`: the filters SQL cannot express (a custom type
/// nested inside the payload, and the direction-dependent cursor).
fn matches_entry_query(entry: &Entry, query: &EntryQuery) -> bool {
    if let Some(entry_type) = query.entry_type {
        if entry.entry_type() != entry_type {
            return false;
        }
    }
    if let Some(custom_type) = &query.custom_type {
        match entry.as_custom() {
            Some(custom) if &custom.custom_type == custom_type => {}
            _ => return false,
        }
    }
    if let Some(cursor) = query.cursor {
        let ok = if query.order_or_default().is_oldest_first() {
            entry.seq > cursor.after_seq
        } else {
            entry.seq < cursor.after_seq
        };
        if !ok {
            return false;
        }
    }
    true
}

fn assert_unused_id(conn: &Connection, session_id: &str, id: &str) -> SessionResult<()> {
    if id_exists_in_entries(conn, session_id, id)? || id_exists_in_records(conn, session_id, id)? {
        return Err(SessionError::already_exists(format!(
            "ID already exists: {id}"
        )));
    }
    Ok(())
}

fn require_session_row(conn: &Connection, session_id: &str) -> SessionResult<SessionRow> {
    read_session_row(conn, session_id)?
        .ok_or_else(|| SessionError::not_found(format!("Session not found: {session_id}")))
}

fn truncate<T>(mut items: Vec<T>, limit: Option<i64>) -> Vec<T> {
    if let Some(limit) = limit {
        items.truncate(limit.max(0) as usize);
    }
    items
}

/// `custom_type` cache column for a freshly provisioned entry.
fn custom_type_of(entry: &ProvisionedEntry) -> Option<String> {
    match &entry.payload {
        pi_session::EntryPayload::Custom(custom) => Some(custom.custom_type.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

struct StorageInner {
    db: SharedConnection,
    session_id: String,
    /// The database path, echoed into `SessionMetadata.extra["path"]`.
    path: String,
    lease: SyncMutex<WriterLease>,
    lease_options: ResolvedWriterLeaseOptions,
    /// Set once the lease is lost; every later write fails with it.
    lease_error: SyncMutex<Option<SessionError>>,
    closing: AtomicBool,
}

impl StorageInner {
    fn renew_in_transaction(&self, conn: &Connection) -> SessionResult<()> {
        let now = now_ms();
        let renewed = {
            let mut lease = self.lease.lock();
            renew_writer_lease(
                conn,
                &self.session_id,
                &mut lease,
                now,
                now + self.lease_options.ttl_ms,
            )?
        };
        if renewed {
            return Ok(());
        }
        let error = lost_writer_error(&self.session_id);
        *self.lease_error.lock() = Some(error.clone());
        Err(error)
    }
}

/// One open SQLite session. Port of upstream's `SqliteSessionStorage`.
pub struct SqliteSessionStorage {
    inner: Arc<StorageInner>,
}

impl std::fmt::Debug for SqliteSessionStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSessionStorage")
            .field("session", &self.inner.session_id)
            .finish()
    }
}

impl SqliteSessionStorage {
    fn new(
        db: SharedConnection,
        session_id: String,
        path: String,
        lease: WriterLease,
        lease_options: ResolvedWriterLeaseOptions,
    ) -> Arc<Self> {
        let inner = Arc::new(StorageInner {
            db,
            session_id,
            path,
            lease: SyncMutex::new(lease),
            lease_options,
            lease_error: SyncMutex::new(None),
            closing: AtomicBool::new(false),
        });
        spawn_heartbeat(&inner);
        Arc::new(Self { inner })
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    fn is_for_session(&self, session_id: &str) -> bool {
        self.inner.session_id == session_id
    }

    fn is_closing(&self) -> bool {
        self.inner.closing.load(Ordering::Acquire)
    }

    /// A read: takes the connection but neither opens a transaction nor renews
    /// the lease, matching upstream's unqueued reads.
    async fn read<T, F>(&self, body: F) -> SessionResult<T>
    where
        F: FnOnce(&Connection, &str) -> SessionResult<T> + Send + 'static,
        T: Send + 'static,
    {
        let session_id = self.inner.session_id.clone();
        with_connection(&self.inner.db, move |conn| body(conn, &session_id)).await
    }

    /// A write: one transaction that first renews the writer lease, so losing
    /// the lease aborts the write rather than letting two owners interleave.
    async fn write<T, F>(&self, body: F) -> SessionResult<T>
    where
        F: FnOnce(&Connection, &str) -> SessionResult<T> + Send + 'static,
        T: Send + 'static,
    {
        if self.is_closing() {
            return Err(SessionError::storage(format!(
                "SQLite session {} is closed",
                self.inner.session_id
            )));
        }
        let inner = self.inner.clone();
        let guard: OwnedMutexGuard<Connection> = self.inner.db.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            if let Some(error) = inner.lease_error.lock().clone() {
                return Err(error);
            }
            let conn: &Connection = &guard;
            transaction(conn, |conn| {
                inner.renew_in_transaction(conn)?;
                body(conn, &inner.session_id)
            })
        })
        .await
        .map_err(join_error)?
    }

    /// Releases the writer lease. Idempotent.
    async fn release(&self) -> SessionResult<()> {
        if self.inner.closing.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let inner = self.inner.clone();
        let guard: OwnedMutexGuard<Connection> = self.inner.db.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let conn: &Connection = &guard;
            transaction(conn, |conn| {
                let lease = inner.lease.lock().clone();
                release_writer_lease(conn, &inner.session_id, &lease)
            })
        })
        .await
        .map_err(join_error)?
    }
}

/// Keeps an idle session's lease alive. Holds only a `Weak`, so a dropped
/// storage ends the task on its next tick rather than leaking it.
fn spawn_heartbeat(inner: &Arc<StorageInner>) {
    let weak: Weak<StorageInner> = Arc::downgrade(inner);
    let interval = Duration::from_millis(inner.lease_options.heartbeat_interval_ms.max(1) as u64);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let Some(inner) = weak.upgrade() else { return };
            if inner.closing.load(Ordering::Acquire) || inner.lease_error.lock().is_some() {
                return;
            }
            let guard = inner.db.clone().lock_owned().await;
            let beat = inner.clone();
            // A transient heartbeat failure is retried; every write still
            // verifies ownership transactionally.
            let _ = tokio::task::spawn_blocking(move || {
                let conn: &Connection = &guard;
                transaction(conn, |conn| beat.renew_in_transaction(conn))
            })
            .await;
        }
    });
}

#[async_trait]
impl EntryStore for SqliteSessionStorage {
    async fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> SessionResult<Entry> {
        let entry = entry.clone();
        let lane = lane.to_string();
        self.write(move |conn, session_id| {
            let parent_id = read_lane_head(conn, session_id, &lane)?;
            assert_unused_id(conn, session_id, &entry.id)?;
            let seq = get_next_sequence(conn, session_id)?;
            let committed = Entry {
                id: entry.id.clone(),
                seq,
                parent_id: parent_id.clone(),
                timestamp: now_ms(),
                payload: entry.payload.clone(),
                extra: entry.extra.clone(),
            };
            let entry_type = committed.entry_type();
            let payload = serde_json::to_string(&entry_payload(&committed)?).map_err(|error| {
                SessionError::invalid_payload(format!(
                    "Durable payload is not serializable: {error}"
                ))
            })?;
            insert_entry_row(
                conn,
                session_id,
                &NewEntryRow {
                    seq,
                    id: committed.id.clone(),
                    parent_id: committed.parent_id.clone(),
                    entry_type: entry_type.as_str().to_string(),
                    timestamp: committed.timestamp,
                    payload,
                },
            )?;
            set_lane_leaf(conn, session_id, &lane, Some(&committed.id))?;
            append_entry_to_branch_cache(
                conn,
                session_id,
                &committed.id,
                seq,
                entry_type.as_str(),
                custom_type_of(&entry).as_deref(),
                committed.parent_id.as_deref(),
            )?;
            if entry_type == EntryType::Message {
                increment_message_count(conn, session_id)?;
            }
            advance_sequence(conn, session_id, seq)?;
            Ok(committed)
        })
        .await
    }

    async fn append_record(&self, record: &NewRecord) -> SessionResult<LaneRecord> {
        let record = record.clone();
        self.write(move |conn, session_id| {
            if read_lane(conn, session_id, &record.lane)?.is_none() {
                return Err(SessionError::invalid_lane(format!(
                    "Lane not found: {}",
                    record.lane
                )));
            }
            assert_unused_id(conn, session_id, &record.id)?;
            let seq = get_next_sequence(conn, session_id)?;
            let timestamp = now_ms();
            let record_type = record.record_type();
            if matches!(record.payload, RecordPayload::OperationStarted(_)) {
                start_lane_operation(conn, session_id, &record.lane, &record.id)?;
            }
            let payload = serde_json::to_string(&record).map_err(|error| {
                SessionError::invalid_payload(format!(
                    "Durable payload is not serializable: {error}"
                ))
            })?;
            append_record_row(
                conn,
                session_id,
                &NewRecordRow {
                    seq,
                    id: record.id.clone(),
                    lane: record.lane.clone(),
                    run_id: record_run_id(&record),
                    record_type: record_type.as_str().to_string(),
                    op_kind: record_op_kind(&record),
                    timestamp,
                    payload,
                },
            )?;
            if let RecordPayload::OperationFinished(finished) = &record.payload {
                finish_lane_operation(conn, session_id, &record.lane, &finished.run_id)?;
            }
            if let RecordPayload::Usage(usage) = &record.payload {
                add_usage_to_stats(conn, session_id, &usage.usage)?;
            }
            advance_sequence(conn, session_id, seq)?;
            Ok(LaneRecord {
                id: record.id,
                seq,
                lane: record.lane,
                timestamp,
                payload: record.payload,
                extra: record.extra,
            })
        })
        .await
    }

    async fn get_entry(&self, id: &str) -> SessionResult<Option<Entry>> {
        let id = id.to_string();
        self.read(
            move |conn, session_id| match read_entry_row(conn, session_id, &id)? {
                Some(row) => decode_entry(&row).map(Some),
                None => Ok(None),
            },
        )
        .await
    }

    async fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>> {
        query.validate()?;
        let query = query.clone();
        self.read(move |conn, session_id| {
            // A `customType` filter cannot be pushed into `entries` (the value
            // lives inside the JSON payload), so the SQL narrows to `custom`
            // and the limit is applied after decoding.
            let sql_type = query
                .entry_type
                .or(query.custom_type.as_ref().map(|_| EntryType::Custom));
            let sql_limit = if query.custom_type.is_none() {
                query.limit
            } else {
                None
            };
            let rows = read_entry_rows(
                conn,
                session_id,
                &EntryRowQuery {
                    after_seq: None,
                    cursor: query.cursor.map(|cursor| cursor.after_seq),
                    entry_type: sql_type,
                    order: query.order,
                    limit: sql_limit,
                },
            )?;
            let mut entries = Vec::with_capacity(rows.len());
            for row in &rows {
                let entry = decode_entry(row)?;
                if matches_entry_query(&entry, &query) {
                    entries.push(entry);
                }
            }
            Ok(truncate(entries, query.limit))
        })
        .await
    }

    async fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>> {
        query.validate()?;
        let query = query.clone();
        self.read(move |conn, session_id| {
            let rows = read_record_rows(conn, session_id, &record_row_query(&query))?;
            rows.iter().map(decode_record_row).collect()
        })
        .await
    }

    async fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<i64>,
    ) -> SessionResult<Vec<LaneRecord>> {
        pi_session::validate_limit(limit)?;
        let lane = lane.to_string();
        self.read(move |conn, session_id| {
            let rows = read_open_operation_rows(conn, session_id, &lane, limit)?;
            rows.iter()
                .map(|row| {
                    let record = decode_record_row(row)?;
                    if !matches!(record.payload, RecordPayload::OperationStarted(_)) {
                        return Err(SessionError::storage("Expected operation_started record"));
                    }
                    Ok(record)
                })
                .collect()
        })
        .await
    }

    async fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>> {
        options.validate()?;
        let options = *options;
        self.read(move |conn, session_id| {
            let after_seq = options.after_seq.unwrap_or(0);
            let limit = options.limit;
            let entry_rows = read_entry_rows(
                conn,
                session_id,
                &EntryRowQuery {
                    after_seq: Some(after_seq),
                    order: Some(EntryOrder::OldestFirst),
                    limit,
                    ..Default::default()
                },
            )?;
            let record_rows = read_record_rows(
                conn,
                session_id,
                &RecordRowQuery {
                    after_seq: Some(after_seq),
                    order: Some(EntryOrder::OldestFirst),
                    limit,
                    ..Default::default()
                },
            )?;
            let lane_rows = read_lane_move_rows(conn, session_id, Some(after_seq), limit)?;
            let fact_rows = read_fact_rows(conn, session_id, Some(after_seq), limit)?;

            // Rows are merged and truncated *before* decoding: a corrupt row
            // beyond the requested window must not fail the read.
            let mut merged: Vec<LogRow> = Vec::new();
            merged.extend(entry_rows.into_iter().map(LogRow::Entry));
            merged.extend(record_rows.into_iter().map(LogRow::Record));
            merged.extend(lane_rows.into_iter().map(LogRow::Lane));
            merged.extend(fact_rows.into_iter().map(LogRow::Fact));
            merged.sort_by_key(LogRow::seq);
            truncate(merged, options.limit)
                .iter()
                .map(|row| row.decode(session_id))
                .collect()
        })
        .await
    }

    async fn get_stats(&self) -> SessionResult<SessionStats> {
        self.read(read_stats).await
    }

    async fn get_name(&self) -> SessionResult<Option<String>> {
        self.read(move |conn, session_id| {
            let row = read_latest_fact(conn, session_id, "name", None)?;
            decode_fact_string(
                row.as_ref().and_then(|row| row.value.as_deref()),
                session_id,
                "name",
            )
        })
        .await
    }

    async fn set_name(&self, name: Option<&str>) -> SessionResult<()> {
        let name = name.map(str::to_string);
        self.write(move |conn, session_id| {
            let seq = get_next_sequence(conn, session_id)?;
            let value = encode_fact_string(name.as_deref())?;
            append_fact(conn, session_id, seq, "name", None, value.as_deref())?;
            advance_sequence(conn, session_id, seq)
        })
        .await
    }

    async fn get_label(&self, id: &str) -> SessionResult<Option<String>> {
        let id = id.to_string();
        self.read(move |conn, session_id| {
            let row = read_latest_fact(conn, session_id, "label", Some(&id))?;
            decode_fact_string(
                row.as_ref().and_then(|row| row.value.as_deref()),
                session_id,
                "label",
            )
        })
        .await
    }

    async fn set_label(&self, id: &str, label: Option<&str>) -> SessionResult<()> {
        let id = id.to_string();
        let label = label.map(str::to_string);
        self.write(move |conn, session_id| {
            if read_entry_row(conn, session_id, &id)?.is_none() {
                return Err(SessionError::not_found(format!("Entry not found: {id}")));
            }
            let seq = get_next_sequence(conn, session_id)?;
            let value = encode_fact_string(label.as_deref())?;
            append_fact(conn, session_id, seq, "label", Some(&id), value.as_deref())?;
            advance_sequence(conn, session_id, seq)
        })
        .await
    }
}

#[async_trait]
impl BranchStore for SqliteSessionStorage {
    async fn get_lanes(&self) -> SessionResult<Vec<LanePointer>> {
        self.read(move |conn, session_id| {
            Ok(read_lanes(conn, session_id)?
                .into_iter()
                .map(|row| LanePointer {
                    lane: row.lane,
                    leaf_id: row.leaf_id,
                })
                .collect())
        })
        .await
    }

    async fn create_lane(&self, lane: &str, at: Option<&str>) -> SessionResult<()> {
        let lane = lane.to_string();
        let at = at.map(str::to_string);
        self.write(move |conn, session_id| {
            if read_lane(conn, session_id, &lane)?.is_some() {
                return Err(SessionError::already_exists(format!(
                    "Lane already exists: {lane}"
                )));
            }
            if let Some(at) = &at {
                if read_entry_row(conn, session_id, at)?.is_none() {
                    return Err(SessionError::not_found(format!("Entry not found: {at}")));
                }
            }
            let seq = get_next_sequence(conn, session_id)?;
            insert_lane(conn, session_id, seq, &lane, at.as_deref())?;
            advance_sequence(conn, session_id, seq)
        })
        .await
    }

    async fn move_lane(&self, lane: &str, to: Option<&str>) -> SessionResult<()> {
        let lane = lane.to_string();
        let to = to.map(str::to_string);
        self.write(move |conn, session_id| {
            if read_lane(conn, session_id, &lane)?.is_none() {
                return Err(SessionError::invalid_lane(format!(
                    "Lane not found: {lane}"
                )));
            }
            if let Some(to) = &to {
                if read_entry_row(conn, session_id, to)?.is_none() {
                    return Err(SessionError::not_found(format!("Entry not found: {to}")));
                }
            }
            let seq = get_next_sequence(conn, session_id)?;
            update_lane(conn, session_id, seq, &lane, to.as_deref())?;
            advance_sequence(conn, session_id, seq)
        })
        .await
    }

    async fn find_entries_on_branch(&self, query: &BoundBranchQuery) -> SessionResult<Vec<Entry>> {
        query.validate()?;
        let query = query.clone();
        self.read(move |conn, session_id| {
            let Some(cached) = read_cached_branch(conn, session_id, &query.start)? else {
                if read_entry_row(conn, session_id, &query.start)?.is_none() {
                    return Err(SessionError::not_found(format!(
                        "Entry not found: {}",
                        query.start
                    )));
                }
                return Err(SessionError::invalid_entry(format!(
                    "Branch cache missing entry {}",
                    query.start
                )));
            };
            let rows = query_cached_branch_rows(
                conn,
                session_id,
                &cached,
                &CachedBranchQuery {
                    entry_type: query.query.entry_type,
                    custom_type: query.query.custom_type.clone(),
                    stop_at_type: query.bounds.stop_at_type,
                    stop_at_id: query.bounds.stop_at_id.clone(),
                    cursor: query.query.cursor.map(|cursor| cursor.after_seq),
                    order: query.query.order,
                    limit: query.query.limit,
                },
            )?;
            validate_cached_branch_rows(&rows, &query)?;
            let mut entries = Vec::with_capacity(rows.len());
            for row in &rows {
                let entry = entry_row_from_cached(row)?;
                if matches_entry_query(&entry, &query.query) {
                    entries.push(entry);
                }
            }
            Ok(truncate(entries, query.query.limit))
        })
        .await
    }
}

#[async_trait]
impl SessionStorage for SqliteSessionStorage {
    async fn get_metadata(&self) -> SessionResult<SessionMetadata> {
        let path = self.inner.path.clone();
        self.read(move |conn, session_id| {
            decode_session_metadata(&require_session_row(conn, session_id)?, &path)
        })
        .await
    }

    async fn close(&self) -> SessionResult<()> {
        self.release().await
    }
}

fn record_row_query(query: &RecordQuery) -> RecordRowQuery {
    RecordRowQuery {
        lane: query.lane.clone(),
        record_type: query
            .record_type
            .map(|record_type| record_type.as_str().to_string()),
        run_id: query.run_id.clone(),
        operation_kind: query
            .operation_kind
            .map(|kind| operation_kind_column(kind).to_string()),
        after_seq: query.after_seq,
        order: query.order,
        limit: query.limit,
    }
}

/// Facts are stored JSON-encoded; `None` clears the fact.
fn encode_fact_string(value: Option<&str>) -> SessionResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(value) => serde_json::to_string(value).map(Some).map_err(|error| {
            SessionError::invalid_payload(format!("Fact is not serializable: {error}"))
        }),
    }
}

/// A merged, not-yet-decoded log row.
enum LogRow {
    Entry(EntryRow),
    Record(RecordRow),
    Lane(LaneMoveRow),
    Fact(FactRow),
}

impl LogRow {
    fn seq(&self) -> i64 {
        match self {
            LogRow::Entry(row) => row.seq,
            LogRow::Record(row) => row.seq,
            LogRow::Lane(row) => row.seq,
            LogRow::Fact(row) => row.seq,
        }
    }

    fn decode(&self, session_id: &str) -> SessionResult<LogItem> {
        Ok(match self {
            LogRow::Entry(row) => LogItem::Entry {
                seq: row.seq,
                entry: decode_entry(row)?,
            },
            LogRow::Record(row) => LogItem::Record {
                seq: row.seq,
                record: decode_record(row.seq, row.timestamp, &row.payload)?,
            },
            LogRow::Lane(row) => LogItem::Lane {
                seq: row.seq,
                lane: row.lane.clone(),
                leaf_id: row.leaf_id.clone(),
            },
            LogRow::Fact(row) if row.kind == "name" => LogItem::Name {
                seq: row.seq,
                name: decode_fact_string(row.value.as_deref(), session_id, "name")?,
            },
            LogRow::Fact(row) => LogItem::Label {
                seq: row.seq,
                target_id: row.key.clone().unwrap_or_default(),
                label: decode_fact_string(row.value.as_deref(), session_id, "label")?,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RepoState {
    db: Option<SharedConnection>,
    active: Vec<Arc<SqliteSessionStorage>>,
}

/// SQLite-backed [`SessionRepo`]. Port of `SqliteSessionRepository`.
///
/// One repository owns one database file and one connection. Every session it
/// hands out shares that connection and holds its own writer lease.
pub struct SqliteSessionRepo {
    database_path: PathBuf,
    lease_options: ResolvedWriterLeaseOptions,
    /// Upstream's `SerialOperationQueue`: repository-level operations
    /// (create/open/list/delete/fork/close) run one at a time.
    state: AsyncMutex<RepoState>,
}

impl std::fmt::Debug for SqliteSessionRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSessionRepo")
            .field("database_path", &self.database_path)
            .finish()
    }
}

impl SqliteSessionRepo {
    /// Opens (lazily) the database at `database_path` with default lease timing.
    pub fn new(database_path: impl Into<PathBuf>) -> Self {
        Self::with_writer_lease(database_path, SqliteWriterLeaseOptions::default())
            .expect("default writer-lease options are valid")
    }

    /// Errors: `invalid_payload` when the lease timings are out of range
    /// (upstream throws a `RangeError` from the constructor).
    pub fn with_writer_lease(
        database_path: impl Into<PathBuf>,
        writer_lease: SqliteWriterLeaseOptions,
    ) -> SessionResult<Self> {
        Ok(Self {
            database_path: database_path.into(),
            lease_options: resolve_writer_lease_options(writer_lease)?,
            state: AsyncMutex::new(RepoState::default()),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn resolved_path(&self) -> SessionResult<PathBuf> {
        absolute_path(&self.database_path)
    }

    async fn database(&self, state: &mut RepoState) -> SessionResult<SharedConnection> {
        if let Some(db) = &state.db {
            return Ok(db.clone());
        }
        let path = self.resolved_path()?;
        create_parent_directory(&path).await?;
        let conn = tokio::task::spawn_blocking(move || open_configured_connection(&path))
            .await
            .map_err(join_error)??;
        let db = Arc::new(AsyncMutex::new(conn));
        state.db = Some(db.clone());
        Ok(db)
    }

    /// Closes and forgets every open session for `session_id`.
    async fn release_storages_for_session(state: &mut RepoState, session_id: &str) {
        let mut remaining = Vec::with_capacity(state.active.len());
        for storage in std::mem::take(&mut state.active) {
            if storage.is_for_session(session_id) {
                let _ = storage.release().await;
            } else {
                remaining.push(storage);
            }
        }
        state.active = remaining;
    }

    fn active_storage(
        state: &mut RepoState,
        session_id: &str,
    ) -> Option<Arc<SqliteSessionStorage>> {
        state.active.retain(|storage| !storage.is_closing());
        state
            .active
            .iter()
            .find(|storage| storage.is_for_session(session_id))
            .cloned()
    }

    fn register(
        state: &mut RepoState,
        db: SharedConnection,
        session_id: String,
        path: String,
        lease: WriterLease,
        lease_options: ResolvedWriterLeaseOptions,
    ) -> Session {
        let storage = SqliteSessionStorage::new(db, session_id, path, lease, lease_options);
        state.active.push(storage.clone());
        Session::new(storage)
    }

    /// Rebuilds this session's private branch-read cache from the canonical
    /// entry parent links. Nothing else repairs it: a stale cache is an error.
    pub async fn repair_branch_cache(&self, metadata: &SessionMetadata) -> SessionResult<()> {
        let mut state = self.state.lock().await;
        Self::release_storages_for_session(&mut state, &metadata.id).await;
        let db = self.database(&mut state).await?;
        let session_id = metadata.id.clone();
        let lease_options = self.lease_options;
        with_connection(&db, move |conn| {
            transaction(conn, |conn| {
                let lease = claim_writer_lease(conn, &session_id, lease_options)?;
                require_session_row(conn, &session_id)?;
                rebuild_branch_cache(conn, &session_id)?;
                release_writer_lease(conn, &session_id, &lease)
            })
        })
        .await
    }

    /// Releases every session and drops the connection. Idempotent.
    pub async fn close(&self) -> SessionResult<()> {
        let mut state = self.state.lock().await;
        for storage in std::mem::take(&mut state.active) {
            let _ = storage.release().await;
        }
        state.db = None;
        Ok(())
    }
}

fn claim_writer_lease(
    conn: &Connection,
    session_id: &str,
    options: ResolvedWriterLeaseOptions,
) -> SessionResult<WriterLease> {
    let now = now_ms();
    acquire_writer_lease(
        conn,
        session_id,
        &pi_core::uuidv7(),
        now,
        now + options.ttl_ms,
    )?
    .ok_or_else(|| active_writer_error(session_id))
}

/// The `cwd` every SQLite session row requires.
fn require_cwd(options: &SessionCreateOptions) -> SessionResult<String> {
    options
        .cwd
        .clone()
        .ok_or_else(|| SessionError::invalid_payload("SQLite sessions require a cwd"))
}

#[async_trait]
impl SessionRepo for SqliteSessionRepo {
    async fn create(&self, options: &SessionCreateOptions) -> SessionResult<Session> {
        let mut state = self.state.lock().await;
        let db = self.database(&mut state).await?;
        let path = self.resolved_path()?.display().to_string();
        let id = options.id.clone().unwrap_or_else(pi_core::uuidv7);
        let cwd = require_cwd(options)?;
        let metadata = options.metadata.clone();
        let parent_session_id = options.parent_session_id.clone();
        let lease_options = self.lease_options;
        let session_id = id.clone();

        let lease = with_connection(&db, move |conn| {
            if session_exists(conn, &session_id)? {
                return Err(SessionError::already_exists(format!(
                    "Session already exists: {session_id}"
                )));
            }
            let created_at = now_ms();
            transaction(conn, |conn| {
                insert_session_row(
                    conn,
                    &NewSessionRow {
                        id: session_id.clone(),
                        created_at,
                        cwd,
                        parent_session_id,
                        metadata,
                    },
                )?;
                create_sequence(conn, &session_id, 1)?;
                create_stats(conn, &session_id, 0)?;
                create_initial_lane(conn, &session_id, "main", None)?;
                claim_writer_lease(conn, &session_id, lease_options)
            })
        })
        .await?;

        Ok(Self::register(
            &mut state,
            db,
            id,
            path,
            lease,
            self.lease_options,
        ))
    }

    async fn open(&self, metadata: &SessionMetadata) -> SessionResult<Session> {
        let mut state = self.state.lock().await;
        let db = self.database(&mut state).await?;
        let path = self.resolved_path()?.display().to_string();
        let id = metadata.id.clone();
        let lease_options = self.lease_options;

        if let Some(active) = Self::active_storage(&mut state, &id) {
            // Still validate the lanes, exactly as upstream does on this path.
            let session_id = id.clone();
            with_connection(&db, move |conn| {
                require_session_row(conn, &session_id)?;
                read_lanes(conn, &session_id).map(|_| ())
            })
            .await?;
            return Ok(Session::new(active));
        }

        let session_id = id.clone();
        let lease = with_connection(&db, move |conn| {
            require_session_row(conn, &session_id)?;
            transaction(conn, |conn| {
                let lease = claim_writer_lease(conn, &session_id, lease_options)?;
                require_session_row(conn, &session_id)?;
                read_lanes(conn, &session_id)?;
                Ok(lease)
            })
        })
        .await?;

        Ok(Self::register(
            &mut state,
            db,
            id,
            path,
            lease,
            self.lease_options,
        ))
    }

    async fn list(&self, options: &SessionListOptions) -> SessionResult<Vec<SessionMetadata>> {
        let mut state = self.state.lock().await;
        let path = self.resolved_path()?;
        // Listing a database that was never created is empty, not an error —
        // and must not create the file either.
        if state.db.is_none() && !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(Vec::new());
        }
        let db = self.database(&mut state).await?;
        let display = path.display().to_string();
        let cwd = options.cwd.clone();
        with_connection(&db, move |conn| {
            read_session_rows(conn, cwd.as_deref())?
                .iter()
                .map(|row| decode_session_metadata(row, &display))
                .collect()
        })
        .await
    }

    async fn delete(&self, metadata: &SessionMetadata) -> SessionResult<()> {
        let mut state = self.state.lock().await;
        Self::release_storages_for_session(&mut state, &metadata.id).await;
        let db = self.database(&mut state).await?;
        let session_id = metadata.id.clone();
        let lease_options = self.lease_options;
        with_connection(&db, move |conn| {
            transaction(conn, |conn| {
                if !session_exists(conn, &session_id)? {
                    return delete_writer_lease(conn, &session_id);
                }
                claim_writer_lease(conn, &session_id, lease_options)?;
                delete_branch_cache(conn, &session_id)?;
                delete_fact_rows(conn, &session_id)?;
                delete_lane_rows(conn, &session_id)?;
                delete_record_rows(conn, &session_id)?;
                delete_entry_rows(conn, &session_id)?;
                delete_writer_lease(conn, &session_id)?;
                delete_stats(conn, &session_id)?;
                delete_sequence(conn, &session_id)?;
                delete_session_row(conn, &session_id)
            })
        })
        .await
    }

    async fn fork(
        &self,
        source: &SessionMetadata,
        fork: &ForkOptions,
        create: &SessionCreateOptions,
    ) -> SessionResult<Session> {
        let mut state = self.state.lock().await;
        let db = self.database(&mut state).await?;
        let path = self.resolved_path()?.display().to_string();
        let id = create.id.clone().unwrap_or_else(pi_core::uuidv7);
        let cwd = match create.cwd.clone() {
            Some(cwd) => cwd,
            None => source
                .get_str("cwd")
                .map(str::to_string)
                .ok_or_else(|| SessionError::invalid_payload("SQLite sessions require a cwd"))?,
        };
        let plan = ForkPlan {
            source_id: source.id.clone(),
            target_id: id.clone(),
            cwd,
            parent_session_id: create
                .parent_session_id
                .clone()
                .unwrap_or_else(|| source.id.clone()),
            metadata: create.metadata.clone(),
            options: fork.clone(),
            path: path.clone(),
            lease_options: self.lease_options,
        };

        let lease = with_connection(&db, move |conn| plan.execute(conn)).await?;

        Ok(Self::register(
            &mut state,
            db,
            id,
            path,
            lease,
            self.lease_options,
        ))
    }
}

/// Everything `fork` needs on the blocking worker, in one owned bundle.
struct ForkPlan {
    source_id: String,
    target_id: String,
    cwd: String,
    parent_session_id: String,
    metadata: Option<Map<String, Value>>,
    options: ForkOptions,
    path: String,
    lease_options: ResolvedWriterLeaseOptions,
}

impl ForkPlan {
    fn execute(self, conn: &Connection) -> SessionResult<WriterLease> {
        let source_metadata =
            decode_session_metadata(&require_session_row(conn, &self.source_id)?, &self.path)?;
        if session_exists(conn, &self.target_id)? {
            return Err(SessionError::already_exists(format!(
                "Session already exists: {}",
                self.target_id
            )));
        }

        let is_tree = self.options.scope_or_default() == ForkScope::Tree;
        let mut entries: Vec<EntryRow> = Vec::new();
        let mut lanes: Vec<LanePointer> = Vec::new();
        let mut branch_tips: Vec<String> = Vec::new();
        let mut branch_fork_target: Option<String> = None;

        if is_tree {
            entries.extend(read_entry_rows(
                conn,
                &self.source_id,
                &EntryRowQuery {
                    order: Some(EntryOrder::OldestFirst),
                    ..Default::default()
                },
            )?);
            lanes.extend(
                read_lanes(conn, &self.source_id)?
                    .into_iter()
                    .map(|row| LanePointer {
                        lane: row.lane,
                        leaf_id: row.leaf_id,
                    }),
            );
            branch_tips.extend(read_branch_tip_ids(conn, &self.source_id)?);
        } else {
            let main = read_lane(conn, &self.source_id, "main")?
                .ok_or_else(|| SessionError::invalid_lane("Lane not found: main"))?;
            let selected = self.options.entry_id.clone().or(main.leaf_id);
            if let Some(selected) = selected {
                let target = read_entry_row(conn, &self.source_id, &selected)?
                    .filter(|row| row.entry_type == "message")
                    .ok_or_else(|| {
                        SessionError::invalid_fork_target(format!(
                            "Fork target is not a message entry: {selected}"
                        ))
                    })?;
                let position = self.options.position.unwrap_or({
                    if self.options.entry_id.is_none() {
                        ForkPosition::At
                    } else {
                        ForkPosition::Before
                    }
                });
                branch_fork_target = match position {
                    ForkPosition::At => Some(target.id.clone()),
                    ForkPosition::Before => target.parent_id.clone(),
                };
            }
            lanes.push(LanePointer {
                lane: "main".into(),
                leaf_id: branch_fork_target.clone(),
            });
            if let Some(target) = &branch_fork_target {
                let cached =
                    read_cached_branch(conn, &self.source_id, target)?.ok_or_else(|| {
                        SessionError::invalid_fork_target(format!(
                            "Fork target is not on a cached branch: {target}"
                        ))
                    })?;
                let rows = query_cached_branch_rows(
                    conn,
                    &self.source_id,
                    &cached,
                    &CachedBranchQuery {
                        order: Some(EntryOrder::OldestFirst),
                        ..Default::default()
                    },
                )?;
                entries.extend(rows.into_iter().map(|row| EntryRow {
                    session_id: self.source_id.clone(),
                    seq: row.entry_seq,
                    id: row.id,
                    parent_id: row.parent_id,
                    entry_type: row.entry_type,
                    timestamp: row.timestamp,
                    payload: row.payload,
                }));
                branch_tips.push(target.clone());
            }
        }

        let copied_ids: HashSet<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        let latest_name = read_latest_fact(conn, &self.source_id, "name", None)?;
        let labels_to_copy: Vec<(String, String)> = read_latest_label_facts(conn, &self.source_id)?
            .into_iter()
            .filter(|(key, _)| is_tree || copied_ids.contains(key.as_str()))
            .collect();
        let created_at = now_ms();
        let metadata = self.metadata.clone().or_else(|| {
            source_metadata
                .get("metadata")
                .and_then(|value| value.as_object().cloned())
        });
        let message_count = entries
            .iter()
            .filter(|entry| entry.entry_type == "message")
            .count() as i64;

        transaction(conn, |conn| {
            insert_session_row(
                conn,
                &NewSessionRow {
                    id: self.target_id.clone(),
                    created_at,
                    cwd: self.cwd.clone(),
                    parent_session_id: Some(self.parent_session_id.clone()),
                    metadata,
                },
            )?;
            create_sequence(conn, &self.target_id, 1)?;
            create_stats(conn, &self.target_id, message_count)?;

            let mut next_seq = 1;
            let mut allocate = || {
                let seq = next_seq;
                next_seq += 1;
                seq
            };
            for entry in &entries {
                insert_entry_row(
                    conn,
                    &self.target_id,
                    &NewEntryRow {
                        seq: allocate(),
                        id: entry.id.clone(),
                        parent_id: entry.parent_id.clone(),
                        entry_type: entry.entry_type.clone(),
                        timestamp: entry.timestamp,
                        payload: entry.payload.clone(),
                    },
                )?;
            }

            if is_tree {
                for pointer in &lanes {
                    insert_lane(
                        conn,
                        &self.target_id,
                        allocate(),
                        &pointer.lane,
                        pointer.leaf_id.as_deref(),
                    )?;
                }
            } else {
                create_initial_lane(conn, &self.target_id, "main", branch_fork_target.as_deref())?;
            }

            if let Some(value) = latest_name.as_ref().and_then(|row| row.value.as_deref()) {
                append_fact(conn, &self.target_id, allocate(), "name", None, Some(value))?;
            }
            for (key, value) in &labels_to_copy {
                append_fact(
                    conn,
                    &self.target_id,
                    allocate(),
                    "label",
                    Some(key),
                    Some(value),
                )?;
            }

            set_next_sequence(conn, &self.target_id, next_seq)?;
            for tip in &branch_tips {
                build_cached_branch(conn, &self.target_id, tip)?;
            }
            claim_writer_lease(conn, &self.target_id, self.lease_options)
        })
        .map_err(|error| match error {
            // Only driver failures are rewritten; a `SessionError` from the
            // fork logic keeps its own code, matching upstream.
            error if error.code() != "storage" => error,
            error => SessionError::storage(format!(
                "Failed to fork SQLite session {}: {error}",
                self.target_id
            )),
        })
    }
}
