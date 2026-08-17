//! The session handle callers actually use. Port of `harness/session/session.ts`.
//!
//! `Session` is the `main`-lane view of a [`SessionStorage`]; [`Session::view`]
//! rebinds the branch-scoped operations to another lane. Both are cheap
//! `Arc` clones, and neither caches a lane leaf — upstream is explicit that
//! views must re-read the leaf on every write so concurrent lanes stay correct.

use std::sync::Arc;

use crate::error::{SessionError, SessionResult};
use crate::messages::AgentMessage;
use crate::repo::{IdGenerator, SessionStorage, Uuidv7Generator};
use crate::state::{
    assert_entry_payload_is_durable, assert_json_serializable, assert_record_payload_is_durable,
};
use crate::types::{
    BoundBranchQuery, BranchQuery, Entry, EntryQuery, LanePointer, LaneRecord, LogItem, LogOptions,
    NewRecord, ProvisionedEntry, RecordQuery, RecordType, SessionMetadata, SessionStats,
};

#[derive(Clone)]
pub struct Session {
    storage: Arc<dyn SessionStorage>,
    id_generator: Arc<dyn IdGenerator>,
    lane: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("lane", &self.lane).finish()
    }
}

impl Session {
    pub fn new(storage: Arc<dyn SessionStorage>) -> Self {
        Self {
            storage,
            id_generator: Arc::new(Uuidv7Generator),
            lane: "main".into(),
        }
    }

    pub fn with_id_generator(
        storage: Arc<dyn SessionStorage>,
        id_generator: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            storage,
            id_generator,
            lane: "main".into(),
        }
    }

    /// Rebind the branch-scoped operations to another lane. Session-wide
    /// operations (facts, stats, tree queries) are unaffected.
    pub fn view(&self, lane: impl Into<String>) -> Session {
        Session {
            storage: self.storage.clone(),
            id_generator: self.id_generator.clone(),
            lane: lane.into(),
        }
    }

    pub fn lane(&self) -> &str {
        &self.lane
    }

    pub fn storage(&self) -> &Arc<dyn SessionStorage> {
        &self.storage
    }

    pub fn id_generator(&self) -> &Arc<dyn IdGenerator> {
        &self.id_generator
    }

    pub async fn get_metadata(&self) -> SessionResult<SessionMetadata> {
        self.storage.get_metadata().await
    }

    pub async fn drain(&self) -> SessionResult<()> {
        self.storage.drain().await
    }

    pub async fn close(&self) -> SessionResult<()> {
        self.storage.close().await
    }

    // -- lanes ------------------------------------------------------------

    pub async fn get_lanes(&self) -> SessionResult<Vec<LanePointer>> {
        self.storage.get_lanes().await
    }

    pub async fn create_lane(&self, lane: &str, at: Option<&str>) -> SessionResult<()> {
        self.storage.create_lane(lane, at).await
    }

    pub async fn move_lane(&self, lane: &str, to: Option<&str>) -> SessionResult<()> {
        self.storage.move_lane(lane, to).await
    }

    /// This view's lane leaf, or `None` when the lane is empty.
    pub async fn get_leaf_id(&self) -> SessionResult<Option<String>> {
        self.leaf_id_for_lane(&self.lane).await
    }

    async fn leaf_id_for_lane(&self, lane: &str) -> SessionResult<Option<String>> {
        self.storage
            .get_lanes()
            .await?
            .into_iter()
            .find(|pointer| pointer.lane == lane)
            .map(|pointer| pointer.leaf_id)
            .ok_or_else(|| SessionError::invalid_lane(format!("Lane not found: {lane}")))
    }

    // -- writes -----------------------------------------------------------

    pub async fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> SessionResult<Entry> {
        assert_entry_payload_is_durable(&entry.payload)?;
        assert_json_serializable(&serde_json::to_value(entry).map_err(|error| {
            SessionError::invalid_payload(format!("Durable payload is not serializable: {error}"))
        })?)?;
        self.storage.append_entry(entry, lane).await
    }

    pub async fn append_record(&self, record: &NewRecord) -> SessionResult<LaneRecord> {
        assert_record_payload_is_durable(&record.payload)?;
        assert_json_serializable(&serde_json::to_value(record).map_err(|error| {
            SessionError::invalid_payload(format!("Durable payload is not serializable: {error}"))
        })?)?;
        self.storage.append_record(record).await
    }

    /// Append a message to this view's lane, returning the new entry id.
    pub async fn append_message(&self, message: AgentMessage) -> SessionResult<String> {
        let entry = ProvisionedEntry::message(self.id_generator.next(), message);
        Ok(self.append_entry(&entry, &self.lane.clone()).await?.id)
    }

    pub async fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Option<serde_json::Value>,
    ) -> SessionResult<String> {
        let entry = ProvisionedEntry::custom(self.id_generator.next(), custom_type, data);
        Ok(self.append_entry(&entry, &self.lane.clone()).await?.id)
    }

    // -- reads ------------------------------------------------------------

    pub async fn get_entry(&self, id: &str) -> SessionResult<Option<Entry>> {
        self.storage.get_entry(id).await
    }

    pub async fn get_stats(&self) -> SessionResult<SessionStats> {
        self.storage.get_stats().await
    }

    pub async fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>> {
        query.validate()?;
        self.storage.find_entries(query).await
    }

    pub async fn find_entry(&self, query: &EntryQuery) -> SessionResult<Option<Entry>> {
        query.validate()?;
        let mut bounded = query.clone();
        bounded.limit = Some(1);
        Ok(self
            .storage
            .find_entries(&bounded)
            .await?
            .into_iter()
            .next())
    }

    pub async fn find_entries_on_branch(&self, query: &BranchQuery) -> SessionResult<Vec<Entry>> {
        match self.resolve_branch_query(query, None).await? {
            Some(bound) => self.storage.find_entries_on_branch(&bound).await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn find_entry_on_branch(&self, query: &BranchQuery) -> SessionResult<Option<Entry>> {
        match self.resolve_branch_query(query, Some(1)).await? {
            Some(bound) => Ok(self
                .storage
                .find_entries_on_branch(&bound)
                .await?
                .into_iter()
                .next()),
            None => Ok(None),
        }
    }

    /// `result_limit` lets single-entry queries cap results without mutating
    /// the caller's query, exactly as upstream's `queryBranchEntries` does.
    async fn resolve_branch_query(
        &self,
        query: &BranchQuery,
        result_limit: Option<i64>,
    ) -> SessionResult<Option<BoundBranchQuery>> {
        query.validate()?;
        let start = match &query.bounds.start {
            Some(start) => Some(start.clone()),
            None => self.leaf_id_for_lane(&self.lane).await?,
        };
        let Some(start) = start else { return Ok(None) };
        let mut bound = query.clone().resolve(start);
        if let Some(limit) = result_limit {
            bound.query.limit = Some(limit);
        }
        Ok(Some(bound))
    }

    pub async fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>> {
        query.validate()?;
        if query.operation_kind.is_some() && query.record_type != Some(RecordType::OperationStarted)
        {
            return Err(SessionError::invalid_query(
                r#"operationKind requires type "operation_started""#,
            ));
        }
        self.storage.find_records(query).await
    }

    pub async fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<i64>,
    ) -> SessionResult<Vec<LaneRecord>> {
        crate::types::validate_limit(limit)?;
        self.storage.find_open_operations(lane, limit).await
    }

    pub async fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>> {
        options.validate()?;
        self.storage.get_log(options).await
    }

    // -- global facts -----------------------------------------------------

    pub async fn get_name(&self) -> SessionResult<Option<String>> {
        self.storage.get_name().await
    }

    pub async fn set_name(&self, name: Option<&str>) -> SessionResult<()> {
        self.storage.set_name(name).await
    }

    pub async fn get_label(&self, target_id: &str) -> SessionResult<Option<String>> {
        self.storage.get_label(target_id).await
    }

    pub async fn set_label(&self, target_id: &str, label: Option<&str>) -> SessionResult<()> {
        self.storage.set_label(target_id, label).await
    }
}
