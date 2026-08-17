//! Volatile backend. Port of `harness/session/memory.ts`.
//!
//! The reference implementation of the storage traits: it is nothing but a
//! [`SessionState`] behind a mutex, so it defines the behaviour the JSONL and
//! SQLite backends have to reproduce.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::now_ms;

use crate::error::{SessionError, SessionResult};
use crate::repo::{BranchStore, EntryStore, SessionRepo, SessionStorage};
use crate::session::Session;
use crate::state::{check_single_open_operation, SessionMutation, SessionState};
use crate::types::{
    BoundBranchQuery, Entry, EntryQuery, ForkOptions, LanePointer, LaneRecord, LogItem, LogOptions,
    NewRecord, ProvisionedEntry, RecordQuery, SessionCreateOptions, SessionListOptions,
    SessionMetadata, SessionStats,
};

pub struct InMemorySessionStorage {
    metadata: SessionMetadata,
    state: Mutex<SessionState>,
}

impl InMemorySessionStorage {
    pub fn new(metadata: SessionMetadata) -> Self {
        Self {
            metadata,
            state: Mutex::new(SessionState::new()),
        }
    }

    /// Copy this session's entries, lanes and facts into a fresh store.
    pub fn fork(
        &self,
        metadata: SessionMetadata,
        options: &ForkOptions,
    ) -> SessionResult<InMemorySessionStorage> {
        let mutations = self.state.lock().create_fork_mutations(options)?;
        let storage = InMemorySessionStorage::new(metadata);
        {
            let mut state = storage.state.lock();
            for mutation in &mutations {
                state.apply_mutation(mutation)?;
            }
        }
        Ok(storage)
    }
}

#[async_trait]
impl EntryStore for InMemorySessionStorage {
    async fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> SessionResult<Entry> {
        let mut state = self.state.lock();
        let parent_id = state.require_lane(lane)?;
        state.validate_unused_id(&entry.id)?;
        let committed = Entry {
            id: entry.id.clone(),
            seq: state.next_sequence(),
            parent_id,
            timestamp: now_ms(),
            payload: entry.payload.clone(),
            extra: entry.extra.clone(),
        };
        state.apply_mutation(&SessionMutation::Entry {
            lane: Some(lane.to_string()),
            entry: committed.clone(),
        })?;
        Ok(committed)
    }

    async fn append_record(&self, record: &NewRecord) -> SessionResult<LaneRecord> {
        let mut state = self.state.lock();
        state.require_lane(&record.lane)?;
        state.validate_unused_id(&record.id)?;
        check_single_open_operation(&state, record)?;
        let committed = LaneRecord {
            id: record.id.clone(),
            seq: state.next_sequence(),
            lane: record.lane.clone(),
            timestamp: now_ms(),
            payload: record.payload.clone(),
            extra: record.extra.clone(),
        };
        state.apply_mutation(&SessionMutation::Record {
            record: committed.clone(),
        })?;
        Ok(committed)
    }

    async fn get_entry(&self, id: &str) -> SessionResult<Option<Entry>> {
        Ok(self.state.lock().get_entry(id).cloned())
    }

    async fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>> {
        self.state.lock().find_entries(query)
    }

    async fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>> {
        self.state.lock().find_records(query)
    }

    async fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<i64>,
    ) -> SessionResult<Vec<LaneRecord>> {
        self.state.lock().find_open_operations(lane, limit)
    }

    async fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>> {
        self.state.lock().get_log(options)
    }

    async fn get_stats(&self) -> SessionResult<SessionStats> {
        Ok(self.state.lock().get_stats())
    }

    async fn get_name(&self) -> SessionResult<Option<String>> {
        Ok(self.state.lock().get_name())
    }

    async fn set_name(&self, name: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock();
        let seq = state.next_sequence();
        state.apply_mutation(&SessionMutation::Name {
            seq,
            name: name.map(str::to_string),
        })
    }

    async fn get_label(&self, id: &str) -> SessionResult<Option<String>> {
        Ok(self.state.lock().get_label(id))
    }

    async fn set_label(&self, id: &str, label: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock();
        state.validate_target(Some(id))?;
        let seq = state.next_sequence();
        state.apply_mutation(&SessionMutation::Label {
            seq,
            target_id: id.to_string(),
            label: label.map(str::to_string),
        })
    }
}

#[async_trait]
impl BranchStore for InMemorySessionStorage {
    async fn get_lanes(&self) -> SessionResult<Vec<LanePointer>> {
        Ok(self.state.lock().get_lanes())
    }

    async fn create_lane(&self, lane: &str, at: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock();
        state.validate_new_lane(lane)?;
        state.validate_target(at)?;
        let seq = state.next_sequence();
        state.apply_mutation(&SessionMutation::Lane {
            seq,
            lane: lane.to_string(),
            leaf_id: at.map(str::to_string),
        })
    }

    async fn move_lane(&self, lane: &str, to: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock();
        state.require_lane(lane)?;
        state.validate_target(to)?;
        let seq = state.next_sequence();
        state.apply_mutation(&SessionMutation::Lane {
            seq,
            lane: lane.to_string(),
            leaf_id: to.map(str::to_string),
        })
    }

    async fn find_entries_on_branch(&self, query: &BoundBranchQuery) -> SessionResult<Vec<Entry>> {
        self.state.lock().find_entries_on_branch(query)
    }
}

#[async_trait]
impl SessionStorage for InMemorySessionStorage {
    async fn get_metadata(&self) -> SessionResult<SessionMetadata> {
        Ok(self.metadata.clone())
    }
}

#[derive(Default)]
pub struct InMemorySessionRepo {
    /// Insertion-ordered so `list` is stable.
    sessions: Mutex<Vec<(String, Arc<InMemorySessionStorage>)>>,
}

impl InMemorySessionRepo {
    pub fn new() -> Self {
        Self::default()
    }

    fn require_storage(&self, id: &str) -> SessionResult<Arc<InMemorySessionStorage>> {
        self.sessions
            .lock()
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, storage)| storage.clone())
            .ok_or_else(|| SessionError::not_found(format!("Session not found: {id}")))
    }

    fn insert(&self, id: String, storage: Arc<InMemorySessionStorage>) -> SessionResult<()> {
        let mut sessions = self.sessions.lock();
        if sessions.iter().any(|(candidate, _)| candidate == &id) {
            return Err(SessionError::already_exists(format!(
                "Session already exists: {id}"
            )));
        }
        sessions.push((id, storage));
        Ok(())
    }
}

#[async_trait]
impl SessionRepo for InMemorySessionRepo {
    async fn create(&self, options: &SessionCreateOptions) -> SessionResult<Session> {
        let id = options.id.clone().unwrap_or_else(pi_core::uuidv7);
        if self
            .sessions
            .lock()
            .iter()
            .any(|(candidate, _)| candidate == &id)
        {
            return Err(SessionError::already_exists(format!(
                "Session already exists: {id}"
            )));
        }
        let mut metadata = SessionMetadata::new(id.clone(), now_ms());
        metadata.parent_session_id = options.parent_session_id.clone();
        let storage = Arc::new(InMemorySessionStorage::new(metadata));
        self.insert(id, storage.clone())?;
        Ok(Session::new(storage))
    }

    async fn open(&self, metadata: &SessionMetadata) -> SessionResult<Session> {
        Ok(Session::new(self.require_storage(&metadata.id)?))
    }

    async fn list(&self, _options: &SessionListOptions) -> SessionResult<Vec<SessionMetadata>> {
        let sessions = self.sessions.lock();
        Ok(sessions
            .iter()
            .map(|(_, storage)| storage.metadata.clone())
            .collect())
    }

    async fn delete(&self, metadata: &SessionMetadata) -> SessionResult<()> {
        self.sessions.lock().retain(|(id, _)| id != &metadata.id);
        Ok(())
    }

    async fn fork(
        &self,
        source: &SessionMetadata,
        fork: &ForkOptions,
        create: &SessionCreateOptions,
    ) -> SessionResult<Session> {
        let source_storage = self.require_storage(&source.id)?;
        let id = create.id.clone().unwrap_or_else(pi_core::uuidv7);
        if self
            .sessions
            .lock()
            .iter()
            .any(|(candidate, _)| candidate == &id)
        {
            return Err(SessionError::already_exists(format!(
                "Session already exists: {id}"
            )));
        }
        let mut metadata = SessionMetadata::new(id.clone(), now_ms());
        metadata.parent_session_id = create
            .parent_session_id
            .clone()
            .or_else(|| Some(source.id.clone()));
        let storage = Arc::new(source_storage.fork(metadata, fork)?);
        self.insert(id, storage.clone())?;
        Ok(Session::new(storage))
    }
}
