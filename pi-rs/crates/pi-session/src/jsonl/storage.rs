//! Append-only per-session JSONL file. Port of `harness/session/jsonl/storage.ts`.
//!
//! Upstream reaches the disk through the harness `FileSystem` abstraction,
//! which `pi-tools` (W9) owns. This port talks to `tokio::fs` directly rather
//! than depending on a crate that does not exist yet; swapping in the trait
//! later is a local change to this file.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use pi_core::now_ms;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::{
    invalid_file, JsonlDecodeError, JsonlDecodeErrorKind, SessionError, SessionResult,
};
use crate::jsonl::codec::{
    encode_header, encode_mutation, metadata_from_header, parse_header, parse_mutation,
    JsonlV4Header,
};
use crate::repo::{BranchStore, EntryStore, SessionStorage};
use crate::state::{check_single_open_operation, SessionMutation, SessionState};
use crate::types::{
    BoundBranchQuery, Entry, EntryQuery, ForkOptions, LanePointer, LaneRecord, LogItem, LogOptions,
    NewRecord, ProvisionedEntry, RecordQuery, SessionMetadata, SessionStats,
};

fn io_error(message: impl AsRef<str>, error: &std::io::Error) -> SessionError {
    let code_is_missing = error.kind() == std::io::ErrorKind::NotFound;
    let text = format!("{}: {error}", message.as_ref());
    if code_is_missing {
        SessionError::not_found(text)
    } else {
        SessionError::storage(text)
    }
}

pub(crate) async fn modified_at_ms(path: &Path) -> SessionResult<i64> {
    let metadata = tokio::fs::metadata(path).await.map_err(|error| {
        io_error(
            format!("Failed to read session metadata {}", path.display()),
            &error,
        )
    })?;
    let modified = metadata.modified().map_err(|error| {
        io_error(
            format!("Failed to read session metadata {}", path.display()),
            &error,
        )
    })?;
    Ok(modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default())
}

/// Build a complete sibling temporary file, then atomically rename it over the
/// destination. The destination is untouched until the rename commits, so a
/// crash while populating leaves only the ignored `.tmp` file behind.
async fn publish_file_atomically<F, Fut>(destination: &Path, populate: F) -> SessionResult<()>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: std::future::Future<Output = SessionResult<()>>,
{
    let temp_path = PathBuf::from(format!("{}.tmp", destination.display()));
    let result = populate(temp_path.clone()).await;
    let result = match result {
        Ok(()) => tokio::fs::rename(&temp_path, destination)
            .await
            .map_err(|error| {
                io_error(
                    format!("Failed to publish staged file {}", destination.display()),
                    &error,
                )
            }),
        Err(error) => Err(error),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    result
}

async fn append_text(path: &Path, text: &str) -> SessionResult<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|error| {
            io_error(
                format!("Failed to append session {}", path.display()),
                &error,
            )
        })?;
    file.write_all(text.as_bytes()).await.map_err(|error| {
        io_error(
            format!("Failed to append session {}", path.display()),
            &error,
        )
    })?;
    file.flush().await.map_err(|error| {
        io_error(
            format!("Failed to append session {}", path.display()),
            &error,
        )
    })
}

pub struct JsonlSessionStorage {
    path: PathBuf,
    metadata: SessionMetadata,
    /// One async mutex guards both the in-memory projection and the file, so a
    /// commit is append-then-apply with no interleaving. This is the port of
    /// upstream's single promise-chain `enqueue`.
    state: Mutex<SessionState>,
}

impl JsonlSessionStorage {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Metadata is immutable for the life of the handle, so it needs no lock.
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub async fn create(path: &Path, header: &JsonlV4Header) -> SessionResult<JsonlSessionStorage> {
        tokio::fs::write(path, encode_header(header))
            .await
            .map_err(|error| {
                io_error(
                    format!("Failed to initialize session {}", path.display()),
                    &error,
                )
            })?;
        let modified_at = modified_at_ms(path).await?;
        Ok(JsonlSessionStorage {
            metadata: metadata_from_header(header, &path.display().to_string(), modified_at),
            path: path.to_path_buf(),
            state: Mutex::new(SessionState::new()),
        })
    }

    pub async fn load(path: &Path) -> SessionResult<JsonlSessionStorage> {
        let path_text = path.display().to_string();
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|error| io_error(format!("Failed to read session {path_text}"), &error))?;
        let mut physical_lines: Vec<&str> = content.split('\n').collect();
        if physical_lines.last() == Some(&"") {
            physical_lines.pop();
        }
        if physical_lines.first().copied().unwrap_or("").is_empty() {
            return Err(invalid_file(
                &path_text,
                1,
                &JsonlDecodeError::schema("is missing a header"),
            ));
        }
        let header =
            parse_header(physical_lines[0]).map_err(|error| invalid_file(&path_text, 1, &error))?;
        let modified_at = modified_at_ms(path).await?;
        let storage = JsonlSessionStorage {
            metadata: metadata_from_header(&header, &path_text, modified_at),
            path: path.to_path_buf(),
            state: Mutex::new(SessionState::new()),
        };

        {
            let mut state = storage.state.lock().await;
            for index in 1..physical_lines.len() {
                let line = physical_lines[index];
                let mutation = match parse_mutation(line) {
                    Ok(mutation) => mutation,
                    Err(error) => {
                        let is_torn_tail = index == physical_lines.len() - 1
                            && error.kind == JsonlDecodeErrorKind::Syntax;
                        if is_torn_tail {
                            // Drop the unacknowledged partial append by
                            // atomically publishing the valid prefix.
                            let valid_prefix = format!("{}\n", physical_lines[..index].join("\n"));
                            drop(state);
                            publish_file_atomically(path, |temp_path| async move {
                                tokio::fs::write(&temp_path, valid_prefix)
                                    .await
                                    .map_err(|error| {
                                        io_error(
                                            format!("Failed to stage torn-tail repair {path_text}"),
                                            &error,
                                        )
                                    })
                            })
                            .await?;
                            return Ok(storage);
                        }
                        return Err(invalid_file(&path_text, index + 1, &error));
                    }
                };
                if let Err(error) = state.apply_mutation(&mutation) {
                    if matches!(error, SessionError::InvalidEntry { .. }) {
                        return Err(invalid_file(
                            &path_text,
                            index + 1,
                            &JsonlDecodeError::schema(error.message()),
                        ));
                    }
                    return Err(error);
                }
            }
        }

        if !content.ends_with('\n') {
            append_text(path, "\n").await?;
        }
        Ok(storage)
    }

    /// Publish a fork of this session at `path`, then reopen it.
    pub async fn fork(
        &self,
        path: &Path,
        header: &JsonlV4Header,
        options: &ForkOptions,
    ) -> SessionResult<JsonlSessionStorage> {
        let mutations = self.state.lock().await.create_fork_mutations(options)?;
        publish_file_atomically(path, |temp_path| async move {
            let target = JsonlSessionStorage::create(&temp_path, header).await?;
            let mut state = target.state.lock().await;
            for mutation in &mutations {
                append_text(&temp_path, &encode_mutation(mutation)?).await?;
                state.apply_mutation(mutation)?;
            }
            Ok(())
        })
        .await?;
        JsonlSessionStorage::load(path).await
    }

    async fn commit(
        &self,
        state: &mut SessionState,
        mutation: &SessionMutation,
    ) -> SessionResult<()> {
        append_text(&self.path, &encode_mutation(mutation)?).await?;
        state.apply_mutation(mutation)
    }
}

#[async_trait]
impl EntryStore for JsonlSessionStorage {
    async fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> SessionResult<Entry> {
        let mut state = self.state.lock().await;
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
        self.commit(
            &mut state,
            &SessionMutation::Entry {
                lane: Some(lane.to_string()),
                entry: committed.clone(),
            },
        )
        .await?;
        Ok(committed)
    }

    async fn append_record(&self, record: &NewRecord) -> SessionResult<LaneRecord> {
        let mut state = self.state.lock().await;
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
        self.commit(
            &mut state,
            &SessionMutation::Record {
                record: committed.clone(),
            },
        )
        .await?;
        Ok(committed)
    }

    async fn get_entry(&self, id: &str) -> SessionResult<Option<Entry>> {
        Ok(self.state.lock().await.get_entry(id).cloned())
    }

    async fn find_entries(&self, query: &EntryQuery) -> SessionResult<Vec<Entry>> {
        self.state.lock().await.find_entries(query)
    }

    async fn find_records(&self, query: &RecordQuery) -> SessionResult<Vec<LaneRecord>> {
        self.state.lock().await.find_records(query)
    }

    async fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<i64>,
    ) -> SessionResult<Vec<LaneRecord>> {
        self.state.lock().await.find_open_operations(lane, limit)
    }

    async fn get_log(&self, options: &LogOptions) -> SessionResult<Vec<LogItem>> {
        self.state.lock().await.get_log(options)
    }

    async fn get_stats(&self) -> SessionResult<SessionStats> {
        Ok(self.state.lock().await.get_stats())
    }

    async fn get_name(&self) -> SessionResult<Option<String>> {
        Ok(self.state.lock().await.get_name())
    }

    async fn set_name(&self, name: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock().await;
        let seq = state.next_sequence();
        let mutation = SessionMutation::Name {
            seq,
            name: name.map(str::to_string),
        };
        self.commit(&mut state, &mutation).await
    }

    async fn get_label(&self, id: &str) -> SessionResult<Option<String>> {
        Ok(self.state.lock().await.get_label(id))
    }

    async fn set_label(&self, id: &str, label: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock().await;
        state.validate_target(Some(id))?;
        let seq = state.next_sequence();
        let mutation = SessionMutation::Label {
            seq,
            target_id: id.to_string(),
            label: label.map(str::to_string),
        };
        self.commit(&mut state, &mutation).await
    }
}

#[async_trait]
impl BranchStore for JsonlSessionStorage {
    async fn get_lanes(&self) -> SessionResult<Vec<LanePointer>> {
        Ok(self.state.lock().await.get_lanes())
    }

    async fn create_lane(&self, lane: &str, at: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock().await;
        state.validate_new_lane(lane)?;
        state.validate_target(at)?;
        let seq = state.next_sequence();
        let mutation = SessionMutation::Lane {
            seq,
            lane: lane.to_string(),
            leaf_id: at.map(str::to_string),
        };
        self.commit(&mut state, &mutation).await
    }

    async fn move_lane(&self, lane: &str, to: Option<&str>) -> SessionResult<()> {
        let mut state = self.state.lock().await;
        state.require_lane(lane)?;
        state.validate_target(to)?;
        let seq = state.next_sequence();
        let mutation = SessionMutation::Lane {
            seq,
            lane: lane.to_string(),
            leaf_id: to.map(str::to_string),
        };
        self.commit(&mut state, &mutation).await
    }

    async fn find_entries_on_branch(&self, query: &BoundBranchQuery) -> SessionResult<Vec<Entry>> {
        self.state.lock().await.find_entries_on_branch(query)
    }
}

#[async_trait]
impl SessionStorage for JsonlSessionStorage {
    async fn get_metadata(&self) -> SessionResult<SessionMetadata> {
        Ok(self.metadata.clone())
    }
}
