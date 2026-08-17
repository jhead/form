//! Backend-independent session search. Port of `search/scanning.ts`.
//!
//! The scanning backend is the fallback every store gets for free: it pages
//! through entries with [`EntryStore::find_entries`](crate::repo::EntryStore::find_entries)
//! and does a substring match
//! on the projected text. Real backends (SQLite FTS) implement
//! [`crate::repo::SearchBackend`] directly.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{SessionError, SessionResult};
use crate::repo::{
    throw_if_aborted, SearchBackend, SessionSearchHit, SessionSearchOptions, SessionStorage,
};
use crate::types::{Entry, EntryOrder, EntryQuery, EntryType, SessionMetadata};

const DEFAULT_PAGE_SIZE: i64 = 100;

/// One entry considered for a match.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSearchCandidate {
    pub entry_id: String,
    pub seq: i64,
    pub entry_type: EntryType,
    pub timestamp: i64,
    pub text: String,
    pub label: Option<String>,
}

/// Projects the searchable text for one entry. Defaults to the entry's JSON
/// plus its label, matching upstream's `defaultSearchText`.
pub type SearchTextProjector =
    Arc<dyn Fn(&SessionMetadata, &Entry, Option<&str>) -> String + Send + Sync>;

pub fn default_search_text(
    _metadata: &SessionMetadata,
    entry: &Entry,
    label: Option<&str>,
) -> String {
    let encoded = serde_json::to_string(entry).unwrap_or_default();
    match label {
        Some(label) => format!("{encoded} {label}"),
        None => encoded,
    }
}

#[derive(Clone)]
pub struct ScanningSessionSearch {
    readables: Vec<Arc<dyn SessionStorage>>,
    project_text: SearchTextProjector,
    page_size: i64,
}

impl ScanningSessionSearch {
    pub fn new(readables: Vec<Arc<dyn SessionStorage>>) -> Self {
        Self {
            readables,
            project_text: Arc::new(default_search_text),
            page_size: DEFAULT_PAGE_SIZE,
        }
    }

    pub fn with_projector(mut self, project_text: SearchTextProjector) -> Self {
        self.project_text = project_text;
        self
    }

    pub fn with_page_size(mut self, page_size: i64) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    async fn candidates(
        &self,
        readable: &Arc<dyn SessionStorage>,
        metadata: &SessionMetadata,
        entry_types: Option<&[EntryType]>,
    ) -> SessionResult<Vec<SessionSearchCandidate>> {
        let mut candidates = Vec::new();
        let mut after_seq = 0;
        loop {
            let mut query = EntryQuery::new()
                .with_order(EntryOrder::OldestFirst)
                .with_limit(self.page_size)
                .with_cursor(after_seq);
            // Upstream narrows the storage query only when exactly one type is
            // requested; wider filters are applied in memory.
            if let Some([only]) = entry_types {
                query = query.with_type(*only);
            }
            let entries = readable.find_entries(&query).await?;
            if entries.is_empty() {
                break;
            }
            let count = entries.len() as i64;
            for entry in &entries {
                if let Some(types) = entry_types {
                    if !types.contains(&entry.entry_type()) {
                        continue;
                    }
                }
                let label = readable.get_label(&entry.id).await?;
                candidates.push(SessionSearchCandidate {
                    entry_id: entry.id.clone(),
                    seq: entry.seq,
                    entry_type: entry.entry_type(),
                    timestamp: entry.timestamp,
                    text: (self.project_text)(metadata, entry, label.as_deref()),
                    label,
                });
            }
            after_seq = entries.last().map(|entry| entry.seq).unwrap_or(after_seq);
            if count < self.page_size {
                break;
            }
        }
        Ok(candidates)
    }
}

#[async_trait]
impl SearchBackend for ScanningSessionSearch {
    async fn search(
        &self,
        text: &str,
        options: &SessionSearchOptions,
    ) -> SessionResult<Vec<SessionSearchHit>> {
        let needle = text.trim().to_lowercase();
        if needle.is_empty() || options.limit.is_some_and(|limit| limit <= 0) {
            return Ok(Vec::new());
        }
        if options
            .entry_types
            .as_ref()
            .is_some_and(|types| types.is_empty())
        {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        let mut seen = HashSet::new();
        for readable in &self.readables {
            throw_if_aborted(options.signal.as_ref())?;
            let metadata = readable.get_metadata().await?;
            if !seen.insert(metadata.id.clone()) {
                return Err(SessionError::storage(format!(
                    "Duplicate sessionId: {}",
                    metadata.id
                )));
            }
            let candidates = self
                .candidates(readable, &metadata, options.entry_types.as_deref())
                .await?;
            for candidate in candidates {
                throw_if_aborted(options.signal.as_ref())?;
                if !candidate.text.to_lowercase().contains(&needle) {
                    continue;
                }
                hits.push(SessionSearchHit {
                    session_id: metadata.id.clone(),
                    entry_id: candidate.entry_id,
                    timestamp: Some(candidate.timestamp),
                    snippet: Some(candidate.text),
                    score: None,
                });
                if options
                    .limit
                    .is_some_and(|limit| hits.len() as i64 >= limit)
                {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    }
}
