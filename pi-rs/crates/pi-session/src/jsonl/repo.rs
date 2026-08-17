//! JSONL session repository. Port of `harness/session/jsonl/repo.ts`.
//!
//! Sessions live at `<root>/--<cwd-with-separators-replaced>--/<iso>_<id>.jsonl`,
//! the same layout the TypeScript coding agent writes, so both implementations
//! can share a sessions directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::now_ms;

use crate::error::{SessionError, SessionResult};
use crate::jsonl::codec::{metadata_from_header, parse_header, JsonlV4Header};
use crate::jsonl::storage::{modified_at_ms, JsonlSessionStorage};
use crate::repo::SessionRepo;
use crate::session::Session;
use crate::state::assert_json_serializable;
use crate::types::{ForkOptions, SessionCreateOptions, SessionListOptions, SessionMetadata};

/// `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`, spelled out to avoid a regex
/// dependency.
fn is_valid_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let alnum = |b: u8| b.is_ascii_alphanumeric();
    match bytes {
        [] => false,
        [only] => alnum(*only),
        [first, middle @ .., last] => {
            alnum(*first)
                && alnum(*last)
                && middle
                    .iter()
                    .all(|b| alnum(*b) || matches!(b, b'.' | b'_' | b'-'))
        }
    }
}

fn validate_session_id(id: &str) -> SessionResult<()> {
    if is_valid_session_id(id) {
        return Ok(());
    }
    Err(SessionError::invalid_payload(
        "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character",
    ))
}

/// `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`
fn session_directory_name(cwd: &str) -> String {
    let trimmed = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    let replaced: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{replaced}--")
}

/// `new Date(createdAt).toISOString().replace(/[:.]/g, "-")` plus `_<id>.jsonl`.
fn session_file_name(created_at: i64, id: &str) -> String {
    let timestamp = chrono::DateTime::from_timestamp_millis(created_at)
        .unwrap_or_else(|| {
            chrono::DateTime::from_timestamp_millis(0).expect("epoch is representable")
        })
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .chars()
        .map(|c| if matches!(c, ':' | '.') { '-' } else { c })
        .collect::<String>();
    format!("{timestamp}_{id}.jsonl")
}

fn absolute(path: &str) -> SessionResult<PathBuf> {
    std::path::absolute(path)
        .map_err(|error| SessionError::storage(format!("Failed to resolve path {path}: {error}")))
}

/// List v4 session metadata under `sessions_root`, newest modification first.
pub async fn list_jsonl_session_metadata(
    sessions_root: &Path,
    options: &SessionListOptions,
) -> SessionResult<Vec<SessionMetadata>> {
    let mut directories = Vec::new();
    match &options.cwd {
        Some(cwd) => {
            let resolved = absolute(cwd)?;
            let directory =
                sessions_root.join(session_directory_name(&resolved.display().to_string()));
            if tokio::fs::metadata(&directory).await.is_ok() {
                directories.push(directory);
            }
        }
        None => {
            if tokio::fs::metadata(sessions_root).await.is_ok() {
                let mut read_dir = tokio::fs::read_dir(sessions_root).await.map_err(|error| {
                    SessionError::storage(format!(
                        "Failed to list sessions directory {}: {error}",
                        sessions_root.display()
                    ))
                })?;
                while let Ok(Some(entry)) = read_dir.next_entry().await {
                    if entry.path().is_dir() {
                        directories.push(entry.path());
                    }
                }
                directories.sort();
            }
        }
    }

    let mut metadata = Vec::new();
    for directory in directories {
        let Ok(mut read_dir) = tokio::fs::read_dir(&directory).await else {
            continue;
        };
        let mut files: Vec<PathBuf> = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if path.is_dir() || path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            files.push(path);
        }
        files.sort();
        for path in files {
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            let Some(first_line) = content.split('\n').next() else {
                continue;
            };
            if first_line.is_empty() {
                continue;
            }
            let Ok(header) = parse_header(first_line) else {
                continue;
            };
            let modified_at = modified_at_ms(&path).await.unwrap_or_default();
            metadata.push(metadata_from_header(
                &header,
                &path.display().to_string(),
                modified_at,
            ));
        }
    }
    metadata.sort_by(|left, right| {
        let left_modified = left
            .get("modifiedAt")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let right_modified = right
            .get("modifiedAt")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        right_modified.cmp(&left_modified)
    });
    Ok(metadata)
}

pub struct JsonlSessionRepo {
    sessions_root: PathBuf,
    /// Prevents same-process create/fork races for one logical `{cwd, id}`:
    /// the durable filename carries a timestamp, so two concurrent creates can
    /// otherwise both decide the destination is free.
    active_create_destinations: Mutex<HashSet<String>>,
}

impl JsonlSessionRepo {
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
            active_create_destinations: Mutex::new(HashSet::new()),
        }
    }

    pub fn sessions_root(&self) -> &Path {
        &self.sessions_root
    }

    fn root(&self) -> SessionResult<PathBuf> {
        absolute(&self.sessions_root.display().to_string())
    }

    fn session_directory(&self, cwd: &str) -> SessionResult<PathBuf> {
        Ok(self.root()?.join(session_directory_name(cwd)))
    }

    fn resolve_destination(
        &self,
        options: &SessionCreateOptions,
    ) -> SessionResult<(String, String)> {
        let id = options.id.clone().unwrap_or_else(pi_core::uuidv7);
        validate_session_id(&id)?;
        let cwd = options
            .cwd
            .clone()
            .ok_or_else(|| SessionError::invalid_payload("JSONL sessions require a cwd"))?;
        Ok((id, absolute(&cwd)?.display().to_string()))
    }

    fn claim(&self, key: &str) -> SessionResult<DestinationClaim<'_>> {
        let mut active = self.active_create_destinations.lock();
        if !active.insert(key.to_string()) {
            return Err(SessionError::already_exists(format!(
                "Session already exists: {key}"
            )));
        }
        Ok(DestinationClaim {
            repo: self,
            key: key.to_string(),
        })
    }

    async fn session_id_exists(&self, id: &str, cwd: &str) -> SessionResult<bool> {
        let suffix = format!("_{id}.jsonl");
        let directory = self.session_directory(cwd)?;
        let Ok(mut read_dir) = tokio::fs::read_dir(&directory).await else {
            return Ok(false);
        };
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                continue;
            }
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn prepare_create(
        &self,
        id: &str,
        cwd: &str,
        options: &SessionCreateOptions,
    ) -> SessionResult<(JsonlV4Header, PathBuf)> {
        if self.session_id_exists(id, cwd).await? {
            return Err(SessionError::already_exists(format!(
                "Session already exists: {id}"
            )));
        }
        let created_at = now_ms();
        let directory = self.session_directory(cwd)?;
        let path = directory.join(session_file_name(created_at, id));
        if let Some(metadata) = &options.metadata {
            assert_json_serializable(&serde_json::Value::Object(metadata.clone()))?;
        }
        let mut header = JsonlV4Header::new(id, created_at, cwd);
        header.parent_session_id = options.parent_session_id.clone();
        header.metadata = options.metadata.clone();
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| {
                SessionError::storage(format!("Failed to create sessions directory: {error}"))
            })?;
        Ok((header, path))
    }

    async fn load_storage(&self, metadata: &SessionMetadata) -> SessionResult<JsonlSessionStorage> {
        let path = metadata.get_str("path").ok_or_else(|| {
            SessionError::not_found(format!("Session not found: {}", metadata.id))
        })?;
        if tokio::fs::metadata(path).await.is_err() {
            return Err(SessionError::not_found(format!(
                "Session not found: {}",
                metadata.id
            )));
        }
        let storage = JsonlSessionStorage::load(Path::new(path)).await?;
        if storage.metadata().id != metadata.id {
            return Err(SessionError::invalid_entry(format!(
                "Session id does not match header: {}",
                metadata.id
            )));
        }
        Ok(storage)
    }
}

/// RAII release for [`JsonlSessionRepo::claim`].
struct DestinationClaim<'a> {
    repo: &'a JsonlSessionRepo,
    key: String,
}

impl Drop for DestinationClaim<'_> {
    fn drop(&mut self) {
        self.repo
            .active_create_destinations
            .lock()
            .remove(&self.key);
    }
}

#[async_trait]
impl SessionRepo for JsonlSessionRepo {
    async fn create(&self, options: &SessionCreateOptions) -> SessionResult<Session> {
        let (id, cwd) = self.resolve_destination(options)?;
        let _claim = self.claim(&format!("{cwd}\0{id}"))?;
        let (header, path) = self.prepare_create(&id, &cwd, options).await?;
        Ok(Session::new(Arc::new(
            JsonlSessionStorage::create(&path, &header).await?,
        )))
    }

    async fn open(&self, metadata: &SessionMetadata) -> SessionResult<Session> {
        Ok(Session::new(Arc::new(self.load_storage(metadata).await?)))
    }

    async fn list(&self, options: &SessionListOptions) -> SessionResult<Vec<SessionMetadata>> {
        list_jsonl_session_metadata(&self.root()?, options).await
    }

    async fn delete(&self, metadata: &SessionMetadata) -> SessionResult<()> {
        if let Some(path) = metadata.get_str("path") {
            match tokio::fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(SessionError::storage(format!(
                        "Failed to delete session {path}: {error}"
                    )))
                }
            }
        }
        Ok(())
    }

    async fn fork(
        &self,
        source: &SessionMetadata,
        fork: &ForkOptions,
        create: &SessionCreateOptions,
    ) -> SessionResult<Session> {
        let source_storage = self.load_storage(source).await?;
        let mut create_options = create.clone();
        create_options.parent_session_id = create
            .parent_session_id
            .clone()
            .or_else(|| Some(source.id.clone()));
        if create_options.cwd.is_none() {
            create_options.cwd = source.get_str("cwd").map(str::to_string);
        }
        let (id, cwd) = self.resolve_destination(&create_options)?;
        let _claim = self.claim(&format!("{cwd}\0{id}"))?;
        let (header, path) = self.prepare_create(&id, &cwd, &create_options).await?;
        Ok(Session::new(Arc::new(
            source_storage.fork(&path, &header, fork).await?,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_match_the_upstream_pattern() {
        for id in ["a", "session", "a.b_c-d", "A1"] {
            assert!(is_valid_session_id(id), "{id} should be valid");
        }
        for id in ["", "-a", "a-", ".a", "a b", "a/b"] {
            assert!(!is_valid_session_id(id), "{id} should be invalid");
        }
    }

    #[test]
    fn directory_and_file_names_match_the_coding_agent_layout() {
        assert_eq!(
            session_directory_name("/workspace/project"),
            "--workspace-project--"
        );
        // Both the drive colon and the separator are replaced, so a Windows cwd
        // yields a doubled dash exactly as upstream's regex does.
        assert_eq!(session_directory_name("C:\\work"), "--C--work--");
        assert_eq!(
            session_file_name(1_700_000_000_000, "abc"),
            "2023-11-14T22-13-20-000Z_abc.jsonl"
        );
    }
}
