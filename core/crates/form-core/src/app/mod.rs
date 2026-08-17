//! Sessions, groups, entries, search, workspace confinement, attachments.
//!
//! **Owner: W1** (`docs/specs/01-core-domain.md`).
//!
//! What is here now is a deliberately minimal in-memory placeholder that exists so the
//! FFI boundary can be proven end to end before the real store lands. W1 replaces it with
//! the SQLite-backed `Store` from the spec, keeping the method signatures below.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use crate::error::{CoreError, Result};
use crate::protocol::{
    now_ms, Entry, EntryKind, ModelRef, ResolvedPath, Session, SessionGroup, SessionList,
    SessionStatus, SessionSummary, ThinkingLevel,
};

pub struct Store {
    #[allow(dead_code)] // TODO(W1): the SQLite store lives here.
    data_dir: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    groups: Vec<SessionGroup>,
    sessions: Vec<SessionSummary>,
    entries: HashMap<String, Vec<Entry>>,
}

impl Store {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            state: Mutex::new(State::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("store poisoned")
    }

    pub fn list_sessions(&self, include_archived: bool) -> Result<SessionList> {
        let state = self.lock();
        Ok(SessionList {
            groups: state.groups.clone(),
            sessions: state
                .sessions
                .iter()
                .filter(|s| include_archived || !s.archived)
                .cloned()
                .collect(),
        })
    }

    pub fn get_session(&self, id: &str) -> Result<Session> {
        let state = self.lock();
        let summary = state
            .sessions
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
        let entries = state.entries.get(id).cloned().unwrap_or_default();
        Ok(Session { summary, entries })
    }

    pub fn create_session(
        &self,
        group_id: Option<String>,
        title: Option<String>,
        workspace_root: Option<String>,
        model_ref: Option<ModelRef>,
    ) -> Result<SessionSummary> {
        let mut state = self.lock();
        let index = state
            .sessions
            .iter()
            .filter(|s| s.group_id == group_id)
            .count() as u32;
        let now = now_ms();
        let summary = SessionSummary {
            id: format!("ses_{}", uuid::Uuid::new_v4().simple()),
            title: title.clone().unwrap_or_else(|| "New chat".to_string()),
            title_is_custom: title.is_some(),
            group_id,
            index,
            workspace_root,
            model_ref: model_ref.unwrap_or_else(default_model_ref),
            status: SessionStatus::Idle,
            message_count: 0,
            total_tokens: 0,
            archived: false,
            pinned: false,
            created_at: now,
            updated_at: now,
        };
        state.sessions.insert(0, summary.clone());
        Ok(summary)
    }

    pub fn append_entry(&self, session_id: &str, kind: EntryKind) -> Result<Entry> {
        let mut state = self.lock();
        if !state.sessions.iter().any(|s| s.id == session_id) {
            return Err(CoreError::SessionNotFound(session_id.to_string()));
        }
        let list = state.entries.entry(session_id.to_string()).or_default();
        let parent_id = list.last().map(|e| e.id.clone());
        let entry = Entry {
            id: format!("ent_{}", uuid::Uuid::new_v4().simple()),
            session_id: session_id.to_string(),
            seq: list.len() as u64,
            parent_id,
            timestamp: now_ms(),
            kind,
        };
        list.push(entry.clone());

        if let Some(s) = state.sessions.iter_mut().find(|s| s.id == session_id) {
            s.message_count += 1;
            s.updated_at = entry.timestamp;
        }
        Ok(entry)
    }

    /// Replace an entry in place, used when a streamed assistant message finalizes.
    pub fn replace_entry(&self, entry: &Entry) -> Result<()> {
        let mut state = self.lock();
        let list = state
            .entries
            .get_mut(&entry.session_id)
            .ok_or_else(|| CoreError::SessionNotFound(entry.session_id.clone()))?;
        if let Some(slot) = list.iter_mut().find(|e| e.id == entry.id) {
            *slot = entry.clone();
        }
        Ok(())
    }

    pub fn set_status(&self, session_id: &str, status: SessionStatus) -> Result<SessionSummary> {
        let mut state = self.lock();
        let s = state
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .ok_or_else(|| CoreError::SessionNotFound(session_id.to_string()))?;
        s.status = status;
        s.updated_at = now_ms();
        Ok(s.clone())
    }

    pub fn add_tokens(&self, session_id: &str, tokens: u64) -> Result<SessionSummary> {
        let mut state = self.lock();
        let s = state
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .ok_or_else(|| CoreError::SessionNotFound(session_id.to_string()))?;
        s.total_tokens += tokens;
        Ok(s.clone())
    }

    /// Derive a title from the first user message unless the user set one (F2.6).
    pub fn maybe_derive_title(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<Option<SessionSummary>> {
        let mut state = self.lock();
        let s = state
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .ok_or_else(|| CoreError::SessionNotFound(session_id.to_string()))?;
        if s.title_is_custom {
            return Ok(None);
        }
        let derived = derive_title(text);
        if derived.is_empty() {
            return Ok(None);
        }
        s.title = derived;
        Ok(Some(s.clone()))
    }
}

pub fn derive_title(text: &str) -> String {
    let first_line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = collapsed.chars().take(60).collect();
    trimmed
        .trim_end_matches(['.', ',', ':', ';', '!', '?'])
        .to_string()
}

pub fn default_model_ref() -> ModelRef {
    ModelRef {
        provider_id: "anthropic".to_string(),
        model_id: "claude-opus-5".to_string(),
        thinking_level: ThinkingLevel::High,
    }
}

/// Workspace confinement (F4.3). This is the API real tools will call, so the rejection
/// classes matter more than the happy path. TODO(W1): symlink resolution + the full test
/// matrix from spec 01 §5.
pub fn resolve_in_workspace(root: Option<&Path>, candidate: &str) -> Result<ResolvedPath> {
    let Some(root) = root else {
        return Ok(ResolvedPath {
            resolved: candidate.to_string(),
            inside_root: false,
        });
    };
    let root = root
        .canonicalize()
        .map_err(|e| CoreError::Io(format!("workspace root: {e}")))?;

    let joined = if Path::new(candidate).is_absolute() {
        PathBuf::from(candidate)
    } else {
        root.join(candidate)
    };

    // Normalize without touching the filesystem so a non-existent path still resolves.
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(CoreError::PathEscapesRoot(candidate.to_string()));
                }
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }

    if !normalized.starts_with(&root) {
        return Err(CoreError::PathEscapesRoot(candidate.to_string()));
    }
    Ok(ResolvedPath {
        resolved: normalized.to_string_lossy().into_owned(),
        inside_root: true,
    })
}
