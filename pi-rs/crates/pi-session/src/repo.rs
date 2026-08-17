//! The storage contract every session backend implements.
//!
//! Port of `SessionStorage` / `SessionRepo` (`harness/session/types.ts`) and
//! `SessionSearch` (`search/index.ts`), cross-checked against the SQLite
//! backend in `packages/session-backends/sqlite-node/src/sqlite/repo.ts`.
//!
//! Upstream splits the surface into one big `SessionStorage` plus a
//! `SessionRepo` factory, both generic over a metadata type. This port keeps
//! the same semantics but obeys the workspace FFI rules:
//!
//! - **No generics or lifetimes in signatures.** Backend-specific metadata
//!   (`cwd`, `path`, `modifiedAt`, `sourceFormat`, ...) rides in
//!   [`SessionMetadata::extra`] instead of a type parameter.
//! - **Object-safe, `Send + Sync`.** Everything is reachable through
//!   `Arc<dyn Trait>`.
//! - **Errors are [`SessionError`]**, whose `code()` strings match upstream's
//!   `SessionErrorCode` exactly; the conformance suite asserts on them.
//!
//! The surface is sliced into four traits so a backend can compose them:
//! [`EntryStore`] (the append-only ledger and its global facts),
//! [`BranchStore`] (lanes and branch-scoped reads), [`SessionStorage`] (the two
//! together plus metadata — one open session), and [`SessionRepo`] (the
//! session lifecycle). [`SearchBackend`] is independent: it reads across
//! sessions and is normally a separate object.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{SessionError, SessionResult};
use crate::session::Session;
use crate::types::{
    BoundBranchQuery, Entry, EntryQuery, EntryType, ForkOptions, LanePointer, LaneRecord, LogItem,
    LogOptions, NewRecord, ProvisionedEntry, RecordQuery, SessionCreateOptions, SessionListOptions,
    SessionMetadata, SessionStats,
};

/// Entry and record ids. Injectable so tests get deterministic ids; the default
/// is UUIDv7, matching upstream's `uuidv7()`.
pub trait IdGenerator: Send + Sync {
    fn next(&self) -> String;
}

/// Default [`IdGenerator`]: `pi_core::uuidv7`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Uuidv7Generator;

impl IdGenerator for Uuidv7Generator {
    fn next(&self) -> String {
        pi_core::uuidv7()
    }
}

/// The append-only ledger: entries, lane records, the durable log, the global
/// facts projected from it, and the running statistics.
///
/// Every write is assigned the next value of one shared, session-wide sequence.
/// Implementations must serialize writes so sequences stay gap-free and the log
/// stays in commit order.
#[async_trait]
pub trait EntryStore: Send + Sync {
    /// Append `entry` to `lane`, chaining it to that lane's current leaf and
    /// advancing the leaf. Assigns `parentId`, `seq` and `timestamp`.
    ///
    /// Errors: `invalid_lane` (no such lane), `already_exists` (id reused by
    /// any entry or record in this session).
    async fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> SessionResult<Entry>;

    /// Append a lane record. Assigns `seq` and `timestamp`. Records never move
    /// a lane leaf.
    ///
    /// Errors: `invalid_lane`, `already_exists`, and `storage` when an
    /// `operation_started` record would open a second operation on a lane that
    /// already has one open.
    async fn append_record(&self, record: &NewRecord) -> SessionResult<LaneRecord>;

    async fn get_entry(&self, id: &str) -> SessionResult<Option<Entry>>;

    /// Session-wide, all branches, sequence order (default newest first).
    async fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>>;

    async fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>>;

    /// Unfinished `operation_started` records for `lane`, newest first.
    ///
    /// Recovery calls this with `limit: Some(2)`: zero results mean the lane is
    /// idle, one means it is suspended, two mean corruption.
    async fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<i64>,
    ) -> SessionResult<Vec<LaneRecord>>;

    /// Every mutation in commit order — entries, records, lane moves and facts.
    async fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>>;

    async fn get_stats(&self) -> SessionResult<SessionStats>;

    // Global facts. Latest wins; not branch-scoped. "set", not "append":
    // append vocabulary is reserved for tree writes.
    async fn get_name(&self) -> SessionResult<Option<String>>;
    async fn set_name(&self, name: Option<&str>) -> SessionResult<()>;
    async fn get_label(&self, id: &str) -> SessionResult<Option<String>>;
    async fn set_label(&self, id: &str, label: Option<&str>) -> SessionResult<()>;
}

/// Lanes (named branch tips) and branch-scoped reads.
///
/// A session always has a `main` lane. Lane names are permanent: once created
/// a lane can be moved but never removed, because its recovery records outlive
/// it.
#[async_trait]
pub trait BranchStore: Send + Sync {
    /// Every lane and its current leaf, in creation order.
    async fn get_lanes(&self) -> SessionResult<Vec<LanePointer>>;

    /// Create `lane` pointing at `at` (`None` for an empty lane).
    ///
    /// Errors: `already_exists`, `not_found` (no such target entry).
    async fn create_lane(&self, lane: &str, at: Option<&str>) -> SessionResult<()>;

    /// Repoint an existing lane. Errors: `invalid_lane`, `not_found`.
    async fn move_lane(&self, lane: &str, to: Option<&str>) -> SessionResult<()>;

    /// Walk from `query.start` toward the root, honouring the query filters and
    /// bounds. Errors: `not_found` when `start` does not exist,
    /// `invalid_entry` when the branch contains a cycle.
    async fn find_entries_on_branch(&self, query: &BoundBranchQuery) -> SessionResult<Vec<Entry>>;
}

/// One open session: the ledger, the lanes, and the session's own metadata.
///
/// Backends acquire whatever writer claim they need when the session is opened
/// (the SQLite backend takes a writer lease) and release it in [`Self::close`].
#[async_trait]
pub trait SessionStorage: EntryStore + BranchStore + Send + Sync {
    async fn get_metadata(&self) -> SessionResult<SessionMetadata>;

    /// Wait for all queued writes to reach durable storage.
    async fn drain(&self) -> SessionResult<()> {
        Ok(())
    }

    /// Release any backend writer claim. Idempotent.
    async fn close(&self) -> SessionResult<()> {
        Ok(())
    }
}

/// Session lifecycle: create, open, list, delete, fork.
#[async_trait]
pub trait SessionRepo: Send + Sync {
    /// Create a new empty session with a single `main` lane.
    /// Errors: `already_exists`, `invalid_payload` (bad id or metadata).
    async fn create(&self, options: &SessionCreateOptions) -> SessionResult<Session>;

    /// Open an existing session for writing and acquire any writer claim.
    /// Errors: `not_found`, `storage` (another writer holds the claim).
    async fn open(&self, metadata: &SessionMetadata) -> SessionResult<Session>;

    /// List session metadata without opening sessions or taking writer claims.
    async fn list(&self, options: &SessionListOptions) -> SessionResult<Vec<SessionMetadata>>;

    /// Delete a session. Idempotent: deleting a missing session succeeds.
    async fn delete(&self, metadata: &SessionMetadata) -> SessionResult<()>;

    /// Copy entries, lanes and facts (never records) from `source` into a new
    /// session. Errors: `not_found`, `already_exists`, `invalid_fork_target`.
    async fn fork(
        &self,
        source: &SessionMetadata,
        fork: &ForkOptions,
        create: &SessionCreateOptions,
    ) -> SessionResult<Session>;
}

/// Search options. Port of `SessionSearchOptions` (`search/index.ts`).
#[derive(Debug, Clone, Default)]
pub struct SessionSearchOptions {
    /// Restrict results to specific canonical entry types.
    pub entry_types: Option<Vec<EntryType>>,
    /// Maximum number of hits.
    pub limit: Option<i64>,
    /// Cancellation, e.g. search-as-you-type.
    pub signal: Option<pi_core::AbortSignal>,
}

/// One search hit. Upstream's base hit is `{ sessionId, entryId }`; the SQLite
/// and scanning backends widen it with a score/snippet, so those live here as
/// optional fields rather than as a type parameter.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchHit {
    pub session_id: String,
    pub entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// Search across sessions.
///
/// Upstream returns an `AsyncIterable`; a `Vec` bounded by
/// [`SessionSearchOptions::limit`] keeps the trait object-safe and bridgeable.
/// Callers that want incremental delivery pass a small limit and page.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn search(
        &self,
        text: &str,
        options: &SessionSearchOptions,
    ) -> SessionResult<Vec<SessionSearchHit>>;
}

/// Shared handles.
pub type SessionRepoRef = Arc<dyn SessionRepo>;
pub type SessionStorageRef = Arc<dyn SessionStorage>;
pub type SearchBackendRef = Arc<dyn SearchBackend>;

/// `throwIfAborted` — the shared abort check search backends use.
pub fn throw_if_aborted(signal: Option<&pi_core::AbortSignal>) -> SessionResult<()> {
    match signal {
        Some(signal) if signal.is_aborted() => {
            Err(SessionError::storage("The operation was aborted"))
        }
        _ => Ok(()),
    }
}
