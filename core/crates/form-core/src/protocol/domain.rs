//! form's own domain types — no `pi` equivalent, free to evolve.
//! See `docs/specs/00-protocol.md` §6.

use serde::{Deserialize, Serialize};

use super::wire::{Cost, Entry, TimestampMs};

// ---------------------------------------------------------------- models

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
    pub thinking_level: ThinkingLevel,
}

// ---------------------------------------------------------------- sessions

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    Idle,
    Streaming,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub title_is_custom: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    pub model_ref: ModelRef,
    pub status: SessionStatus,
    pub message_count: u64,
    pub total_tokens: u64,
    pub archived: bool,
    pub pinned: bool,
    pub created_at: TimestampMs,
    pub updated_at: TimestampMs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionGroup {
    pub id: String,
    pub name: String,
    pub index: u32,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionList {
    pub groups: Vec<SessionGroup>,
    pub sessions: Vec<SessionSummary>,
}

// ---------------------------------------------------------------- search

/// A highlight range within `snippet`, in UTF-16 code units so Swift can apply it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightRange {
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub title: String,
    pub snippet: String,
    pub highlights: Vec<HighlightRange>,
    pub score: f64,
    pub timestamp: TimestampMs,
}

// ---------------------------------------------------------------- context

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SegmentKind {
    System,
    Tools,
    Transcript,
    Attachments,
    OutputReserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSegment {
    pub kind: SegmentKind,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub session_id: String,
    pub used: u64,
    pub total: u64,
    pub segments: Vec<ContextSegment>,
    pub cost: Cost,
    pub message_count: u64,
}

impl ContextUsage {
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.used as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }
}

// ---------------------------------------------------------------- attachments

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub sha256: String,
    pub filename: String,
    pub mime: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb_path: Option<String>,
    pub created_at: TimestampMs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub path: String,
    pub last_used: TimestampMs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPath {
    pub resolved: String,
    pub inside_root: bool,
}

// ---------------------------------------------------------------- run outcome

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunOutcome {
    Completed,
    Aborted,
    Failed,
}
