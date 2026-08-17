//! In-memory projection of a session's durable mutation log.
//!
//! Port of `harness/session/state.ts`. Every backend folds its storage into
//! this structure: the in-memory one *is* this structure, the JSONL one
//! replays its file into it on load, and the SQLite one keeps the same
//! invariants in SQL. Keeping the validation here is what makes the three
//! backends agree on error codes.

use std::collections::{HashMap, HashSet};

use crate::error::{SessionError, SessionResult};
use crate::types::{
    validate_limit, BoundBranchQuery, Entry, EntryOrder, EntryQuery, ForkOptions, ForkPosition,
    ForkScope, LanePointer, LaneRecord, LogItem, LogOptions, RecordPayload, RecordQuery,
    SessionStats,
};

/// One durable change. The JSONL codec encodes exactly this union, one per line.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMutation {
    /// `lane` is `None` for imported entries that do not move a lane leaf.
    Entry {
        lane: Option<String>,
        entry: Entry,
    },
    Record {
        record: LaneRecord,
    },
    Lane {
        seq: i64,
        lane: String,
        leaf_id: Option<String>,
    },
    Name {
        seq: i64,
        name: Option<String>,
    },
    Label {
        seq: i64,
        target_id: String,
        label: Option<String>,
    },
}

impl SessionMutation {
    pub fn seq(&self) -> i64 {
        match self {
            SessionMutation::Entry { entry, .. } => entry.seq,
            SessionMutation::Record { record } => record.seq,
            SessionMutation::Lane { seq, .. }
            | SessionMutation::Name { seq, .. }
            | SessionMutation::Label { seq, .. } => *seq,
        }
    }
}

fn invalid_mutation(message: impl AsRef<str>) -> SessionError {
    SessionError::invalid_entry(format!("Invalid session mutation: {}", message.as_ref()))
}

#[derive(Debug, Default)]
pub struct SessionState {
    sequence: i64,
    used_ids: HashSet<String>,
    entries: Vec<Entry>,
    entries_by_id: HashMap<String, usize>,
    records: Vec<LaneRecord>,
    /// Per lane, the still-open `operation_started` records in insertion order.
    open_operations: Vec<(String, Vec<LaneRecord>)>,
    /// Insertion-ordered lane pointers; `main` always exists.
    lanes: Vec<LanePointer>,
    log: Vec<LogItem>,
    stats: SessionStats,
    name: Option<String>,
    labels: HashMap<String, String>,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            lanes: vec![LanePointer {
                lane: "main".into(),
                leaf_id: None,
            }],
            ..Default::default()
        }
    }

    pub fn next_sequence(&self) -> i64 {
        self.sequence + 1
    }

    pub fn get_lanes(&self) -> Vec<LanePointer> {
        self.lanes.clone()
    }

    fn lane_slot(&self, lane: &str) -> Option<&LanePointer> {
        self.lanes.iter().find(|pointer| pointer.lane == lane)
    }

    /// The lane's leaf, or `invalid_lane` when the lane does not exist.
    pub fn require_lane(&self, lane: &str) -> SessionResult<Option<String>> {
        self.lane_slot(lane)
            .map(|pointer| pointer.leaf_id.clone())
            .ok_or_else(|| SessionError::invalid_lane(format!("Lane not found: {lane}")))
    }

    pub fn validate_new_lane(&self, lane: &str) -> SessionResult<()> {
        match self.lane_slot(lane) {
            Some(_) => Err(SessionError::already_exists(format!(
                "Lane already exists: {lane}"
            ))),
            None => Ok(()),
        }
    }

    pub fn validate_target(&self, target_id: Option<&str>) -> SessionResult<()> {
        match target_id {
            Some(id) if !self.entries_by_id.contains_key(id) => {
                Err(SessionError::not_found(format!("Entry not found: {id}")))
            }
            _ => Ok(()),
        }
    }

    pub fn validate_unused_id(&self, id: &str) -> SessionResult<()> {
        if self.used_ids.contains(id) {
            return Err(SessionError::already_exists(format!(
                "Session id already exists: {id}"
            )));
        }
        Ok(())
    }

    pub fn open_operation_id(&self, lane: &str) -> Option<&str> {
        self.open_operations
            .iter()
            .find(|(candidate, _)| candidate == lane)
            .and_then(|(_, records)| records.last())
            .map(|record| record.id.as_str())
    }

    pub fn apply_mutation(&mut self, mutation: &SessionMutation) -> SessionResult<()> {
        let seq = mutation.seq();
        if seq != self.sequence + 1 {
            return Err(invalid_mutation(format!("has non-consecutive seq {seq}")));
        }

        match mutation {
            SessionMutation::Entry { lane, entry } => {
                if self.used_ids.contains(&entry.id) {
                    return Err(invalid_mutation(format!(
                        "contains duplicate id {}",
                        entry.id
                    )));
                }
                if let Some(lane) = lane {
                    let leaf_id = match self.lane_slot(lane) {
                        Some(pointer) => pointer.leaf_id.clone(),
                        None => {
                            return Err(invalid_mutation(format!("references missing lane {lane}")))
                        }
                    };
                    if entry.parent_id != leaf_id {
                        return Err(invalid_mutation("does not chain to the lane leaf"));
                    }
                }
                if let Some(parent_id) = &entry.parent_id {
                    if !self.entries_by_id.contains_key(parent_id) {
                        return Err(invalid_mutation(format!(
                            "references missing parent {parent_id}"
                        )));
                    }
                }
                self.sequence = seq;
                self.used_ids.insert(entry.id.clone());
                self.entries_by_id
                    .insert(entry.id.clone(), self.entries.len());
                self.entries.push(entry.clone());
                if let Some(lane) = lane {
                    if let Some(pointer) =
                        self.lanes.iter_mut().find(|pointer| &pointer.lane == lane)
                    {
                        pointer.leaf_id = Some(entry.id.clone());
                    }
                }
                self.log.push(LogItem::Entry {
                    seq,
                    entry: entry.clone(),
                });
                if entry.entry_type() == crate::types::EntryType::Message {
                    self.stats.message_count += 1;
                }
            }
            SessionMutation::Record { record } => {
                if self.lane_slot(&record.lane).is_none() {
                    return Err(invalid_mutation(format!(
                        "references missing lane {}",
                        record.lane
                    )));
                }
                if self.used_ids.contains(&record.id) {
                    return Err(invalid_mutation(format!(
                        "contains duplicate id {}",
                        record.id
                    )));
                }
                self.sequence = seq;
                self.used_ids.insert(record.id.clone());
                self.records.push(record.clone());
                match &record.payload {
                    RecordPayload::OperationStarted(_) => self.push_open_operation(record.clone()),
                    RecordPayload::OperationFinished(finished) => {
                        self.close_open_operation(&record.lane, &finished.run_id)
                    }
                    _ => {}
                }
                self.log.push(LogItem::Record {
                    seq,
                    record: record.clone(),
                });
                if let RecordPayload::Usage(usage) = &record.payload {
                    self.stats.cached_tokens += usage.usage.cache_read;
                    self.stats.uncached_tokens += usage.usage.input + usage.usage.cache_write;
                    self.stats.total_tokens += usage.usage.total_tokens;
                    self.stats.cost_total += usage.usage.cost.total;
                }
            }
            SessionMutation::Lane { lane, leaf_id, .. } => {
                if let Some(leaf_id) = leaf_id {
                    if !self.entries_by_id.contains_key(leaf_id) {
                        return Err(invalid_mutation(format!(
                            "references missing lane target {leaf_id}"
                        )));
                    }
                }
                self.sequence = seq;
                match self.lanes.iter_mut().find(|pointer| &pointer.lane == lane) {
                    Some(pointer) => pointer.leaf_id = leaf_id.clone(),
                    None => self.lanes.push(LanePointer {
                        lane: lane.clone(),
                        leaf_id: leaf_id.clone(),
                    }),
                }
                self.log.push(LogItem::Lane {
                    seq,
                    lane: lane.clone(),
                    leaf_id: leaf_id.clone(),
                });
            }
            SessionMutation::Name { name, .. } => {
                self.sequence = seq;
                self.name = name.clone();
                self.log.push(LogItem::Name {
                    seq,
                    name: name.clone(),
                });
            }
            SessionMutation::Label {
                target_id, label, ..
            } => {
                if !self.entries_by_id.contains_key(target_id) {
                    return Err(invalid_mutation(format!(
                        "references missing label target {target_id}"
                    )));
                }
                self.sequence = seq;
                match label {
                    Some(label) => {
                        self.labels.insert(target_id.clone(), label.clone());
                    }
                    None => {
                        self.labels.remove(target_id);
                    }
                }
                self.log.push(LogItem::Label {
                    seq,
                    target_id: target_id.clone(),
                    label: label.clone(),
                });
            }
        }
        Ok(())
    }

    fn push_open_operation(&mut self, record: LaneRecord) {
        let lane = record.lane.clone();
        match self
            .open_operations
            .iter_mut()
            .find(|(name, _)| name == &lane)
        {
            Some((_, records)) => records.push(record),
            None => self.open_operations.push((lane, vec![record])),
        }
    }

    fn close_open_operation(&mut self, lane: &str, run_id: &str) {
        if let Some((_, records)) = self
            .open_operations
            .iter_mut()
            .find(|(name, _)| name == lane)
        {
            records.retain(|record| record.id != run_id);
        }
    }

    pub fn get_entry(&self, id: &str) -> Option<&Entry> {
        self.entries_by_id
            .get(id)
            .map(|index| &self.entries[*index])
    }

    pub fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>> {
        query.validate()?;
        let order = query.order_or_default();
        let mut results = Vec::new();
        let indices: Box<dyn Iterator<Item = usize>> = if order.is_oldest_first() {
            Box::new(0..self.entries.len())
        } else {
            Box::new((0..self.entries.len()).rev())
        };
        for index in indices {
            let entry = &self.entries[index];
            if !matches_entry_query(entry, query, order) {
                continue;
            }
            results.push(entry.clone());
            if Some(results.len() as i64) == query.limit {
                break;
            }
        }
        Ok(results)
    }

    pub fn find_entries_on_branch(&self, query: &BoundBranchQuery) -> SessionResult<Vec<Entry>> {
        query.validate()?;
        let order = query.query.order_or_default();
        let mut results = Vec::new();
        if order.is_oldest_first() {
            // Upstream walks the *unbounded* path and applies the stop bound
            // from the root side, so the bound is inclusive in both orders.
            let mut path = self.walk_to_root(&query.start, None, None)?;
            path.reverse();
            for entry in path {
                let reached_bound = query.bounds.stop_at_id.as_deref() == Some(entry.id.as_str())
                    || query.bounds.stop_at_type == Some(entry.entry_type());
                if matches_entry_query(&entry, &query.query, order) {
                    results.push(entry);
                }
                if reached_bound || Some(results.len() as i64) == query.query.limit {
                    break;
                }
            }
        } else {
            for entry in self.walk_to_root(
                &query.start,
                query.bounds.stop_at_id.as_deref(),
                query.bounds.stop_at_type,
            )? {
                if matches_entry_query(&entry, &query.query, order) {
                    results.push(entry);
                }
                if Some(results.len() as i64) == query.query.limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    pub fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>> {
        query.validate()?;
        let order = query.order_or_default();
        let mut results = Vec::new();
        let indices: Box<dyn Iterator<Item = usize>> = if order.is_oldest_first() {
            Box::new(0..self.records.len())
        } else {
            Box::new((0..self.records.len()).rev())
        };
        for index in indices {
            let record = &self.records[index];
            if !matches_record_query(record, query) {
                continue;
            }
            results.push(record.clone());
            if Some(results.len() as i64) == query.limit {
                break;
            }
        }
        Ok(results)
    }

    pub fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<i64>,
    ) -> SessionResult<Vec<LaneRecord>> {
        validate_limit(limit)?;
        let mut open: Vec<LaneRecord> = self
            .open_operations
            .iter()
            .find(|(name, _)| name == lane)
            .map(|(_, records)| records.clone())
            .unwrap_or_default();
        open.reverse();
        if let Some(limit) = limit {
            open.truncate(limit as usize);
        }
        Ok(open)
    }

    pub fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>> {
        options.validate()?;
        let mut results = Vec::new();
        for item in &self.log {
            if let Some(after_seq) = options.after_seq {
                if item.seq() <= after_seq {
                    continue;
                }
            }
            results.push(item.clone());
            if Some(results.len() as i64) == options.limit {
                break;
            }
        }
        Ok(results)
    }

    pub fn get_name(&self) -> Option<String> {
        self.name.clone()
    }

    pub fn get_label(&self, id: &str) -> Option<String> {
        self.labels.get(id).cloned()
    }

    pub fn get_stats(&self) -> SessionStats {
        self.stats
    }

    /// The mutation list that reproduces this session (or one branch of it) in
    /// a fresh store. Records are deliberately not copied.
    pub fn create_fork_mutations(
        &self,
        options: &ForkOptions,
    ) -> SessionResult<Vec<SessionMutation>> {
        let (copied_entries, fork_lanes) = match options.scope_or_default() {
            ForkScope::Tree => (
                self.find_entries(&EntryQuery::new().with_order(EntryOrder::OldestFirst))?,
                self.get_lanes(),
            ),
            ForkScope::Branch => {
                let selected = match &options.entry_id {
                    Some(id) => Some(id.clone()),
                    None => self.require_lane("main")?,
                };
                let target_id = match selected {
                    None => None,
                    Some(selected) => {
                        let entry = self
                            .get_entry(&selected)
                            .filter(|entry| entry.entry_type() == crate::types::EntryType::Message);
                        let entry = entry.ok_or_else(|| {
                            SessionError::invalid_fork_target(format!(
                                "Fork target is not a message entry: {selected}"
                            ))
                        })?;
                        let position = options.position.unwrap_or(if options.entry_id.is_none() {
                            ForkPosition::At
                        } else {
                            ForkPosition::Before
                        });
                        match position {
                            ForkPosition::At => Some(entry.id.clone()),
                            ForkPosition::Before => entry.parent_id.clone(),
                        }
                    }
                };
                let entries = match &target_id {
                    None => Vec::new(),
                    Some(start) => {
                        let mut query = BoundBranchQuery::new(start);
                        query.query.order = Some(EntryOrder::OldestFirst);
                        self.find_entries_on_branch(&query)?
                    }
                };
                (
                    entries,
                    vec![LanePointer {
                        lane: "main".into(),
                        leaf_id: target_id,
                    }],
                )
            }
        };

        let mut mutations = Vec::new();
        let mut sequence = 1;
        for source_entry in &copied_entries {
            let mut entry = source_entry.clone();
            entry.seq = sequence;
            sequence += 1;
            mutations.push(SessionMutation::Entry { lane: None, entry });
        }
        for pointer in fork_lanes {
            mutations.push(SessionMutation::Lane {
                seq: sequence,
                lane: pointer.lane,
                leaf_id: pointer.leaf_id,
            });
            sequence += 1;
        }
        if let Some(name) = &self.name {
            mutations.push(SessionMutation::Name {
                seq: sequence,
                name: Some(name.clone()),
            });
            sequence += 1;
        }
        for entry in &copied_entries {
            if let Some(label) = self.labels.get(&entry.id) {
                mutations.push(SessionMutation::Label {
                    seq: sequence,
                    target_id: entry.id.clone(),
                    label: Some(label.clone()),
                });
                sequence += 1;
            }
        }
        Ok(mutations)
    }

    fn walk_to_root(
        &self,
        start: &str,
        stop_at_id: Option<&str>,
        stop_at_type: Option<crate::types::EntryType>,
    ) -> SessionResult<Vec<Entry>> {
        let mut visited = HashSet::new();
        let mut path = Vec::new();
        let mut current = self
            .get_entry(start)
            .ok_or_else(|| SessionError::not_found(format!("Entry not found: {start}")))?;
        loop {
            if !visited.insert(current.id.clone()) {
                return Err(SessionError::invalid_entry(format!(
                    "Session branch contains a cycle at {}",
                    current.id
                )));
            }
            path.push(current.clone());
            if stop_at_id == Some(current.id.as_str()) || stop_at_type == Some(current.entry_type())
            {
                break;
            }
            let parent_id = match &current.parent_id {
                Some(parent_id) => parent_id.clone(),
                None => break,
            };
            current = self.get_entry(&parent_id).ok_or_else(|| {
                SessionError::invalid_entry(format!("Entry not found: {parent_id}"))
            })?;
        }
        Ok(path)
    }
}

fn matches_entry_query(entry: &Entry, query: &EntryQuery, order: EntryOrder) -> bool {
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
        let ok = if order.is_oldest_first() {
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

fn matches_record_query(record: &LaneRecord, query: &RecordQuery) -> bool {
    if let Some(lane) = &query.lane {
        if &record.lane != lane {
            return false;
        }
    }
    if let Some(record_type) = query.record_type {
        if record.record_type() != record_type {
            return false;
        }
    }
    if let Some(run_id) = &query.run_id {
        let matches = match &record.payload {
            RecordPayload::OperationStarted(_) => &record.id == run_id,
            other => other.run_id() == Some(run_id.as_str()),
        };
        if !matches {
            return false;
        }
    }
    if let Some(kind) = query.operation_kind {
        match record.as_operation_started() {
            Some(started) if started.intent.kind() == kind => {}
            _ => return false,
        }
    }
    if let Some(after_seq) = query.after_seq {
        if record.seq <= after_seq {
            return false;
        }
    }
    true
}

/// Shared by every backend: reject an `operation_started` that would open a
/// second concurrent operation on a lane.
pub fn check_single_open_operation(
    state: &SessionState,
    record: &crate::types::NewRecord,
) -> SessionResult<()> {
    if !matches!(record.payload, RecordPayload::OperationStarted(_)) {
        return Ok(());
    }
    if let Some(open_id) = state.open_operation_id(&record.lane) {
        return Err(SessionError::storage(format!(
            "Lane {} already has an open operation {open_id}",
            record.lane
        )));
    }
    Ok(())
}

/// Shared by every backend: reject payloads JSON cannot represent.
///
/// Port of `assertJsonSerializable` (`session/session.ts`). Rust's type system
/// already rules out every case upstream checks except one: a non-finite
/// `f64`. That one matters because `serde_json` silently writes `NaN` and
/// `Infinity` as `null`, which then fails to load back as a number — a
/// corrupted session file rather than a rejected write.
///
/// `serde_json::Value` cannot itself hold a non-finite number
/// (`Number::from_f64` rejects them), so this walk is defensive; the real check
/// is `assert_finite_usage` on the typed payload.
pub fn assert_json_serializable(value: &serde_json::Value) -> SessionResult<()> {
    fn walk(value: &serde_json::Value) -> Result<(), &'static str> {
        match value {
            serde_json::Value::Number(number) => {
                if number.as_f64().map(f64::is_finite) == Some(false) {
                    return Err("contains a non-finite number");
                }
                Ok(())
            }
            serde_json::Value::Array(items) => items.iter().try_for_each(walk),
            serde_json::Value::Object(fields) => fields.values().try_for_each(walk),
            _ => Ok(()),
        }
    }
    walk(value).map_err(|reason| SessionError::invalid_payload(format!("Durable payload {reason}")))
}

fn assert_finite_usage(usage: &pi_core::Usage) -> SessionResult<()> {
    let cost = &usage.cost;
    let finite = cost.input.is_finite()
        && cost.output.is_finite()
        && cost.cache_read.is_finite()
        && cost.cache_write.is_finite()
        && cost.total.is_finite();
    if finite {
        return Ok(());
    }
    Err(SessionError::invalid_payload(
        "Durable payload contains a non-finite number",
    ))
}

fn usages_in_message(message: &crate::messages::AgentMessage, out: &mut Vec<pi_core::Usage>) {
    match message {
        crate::messages::AgentMessage::Assistant(assistant) => out.push(assistant.usage.clone()),
        crate::messages::AgentMessage::ToolResult(result) => {
            if let Some(usage) = &result.usage {
                out.push(usage.clone());
            }
        }
        _ => {}
    }
}

/// Every `Usage` an entry payload carries. Floats reach durable storage only
/// through `Usage.cost`, so this is the complete set of values to validate.
pub fn assert_entry_payload_is_durable(payload: &crate::types::EntryPayload) -> SessionResult<()> {
    let mut usages = Vec::new();
    match payload {
        crate::types::EntryPayload::Message(entry) => {
            usages_in_message(&entry.message, &mut usages)
        }
        crate::types::EntryPayload::Compaction(entry) => {
            if let Some(usage) = &entry.usage {
                usages.push(usage.clone());
            }
            for message in &entry.retained_tail {
                usages_in_message(message, &mut usages);
            }
        }
        crate::types::EntryPayload::BranchSummary(entry) => {
            if let Some(usage) = &entry.usage {
                usages.push(usage.clone());
            }
        }
        _ => {}
    }
    usages.iter().try_for_each(assert_finite_usage)
}

/// Every `Usage` a record payload carries, including the ones nested in a
/// provisioned queue/write target.
pub fn assert_record_payload_is_durable(payload: &RecordPayload) -> SessionResult<()> {
    match payload {
        RecordPayload::Usage(record) => assert_finite_usage(&record.usage),
        RecordPayload::QueueEnqueued(record) => {
            assert_entry_payload_is_durable(&record.target.payload)
        }
        RecordPayload::WriteDeferred(record) => {
            assert_entry_payload_is_durable(&record.target.payload)
        }
        RecordPayload::OperationStarted(record) => match &record.intent {
            crate::types::OperationIntent::Run(run) => {
                for message in &run.original_prompt {
                    let mut usages = Vec::new();
                    usages_in_message(message, &mut usages);
                    usages.iter().try_for_each(assert_finite_usage)?;
                }
                run.initial_messages
                    .iter()
                    .try_for_each(|target| assert_entry_payload_is_durable(&target.payload))
            }
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}
