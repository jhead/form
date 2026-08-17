//! The append-only transcript log, its FTS mirror, and branching.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{CoreError, Result};
use crate::protocol::{now_ms, Entry, EntryKind, Message, Session, SessionSummary};

use super::{index_title, insert_session, new_id, shift_group_from, summary_by_id, Store};

impl Store {
    pub fn get_session(&self, id: &str) -> Result<Session> {
        self.with_conn(|conn| {
            let summary = summary_by_id(conn, id)?;
            Ok(Session {
                summary,
                entries: read_entries(conn, id, None)?,
            })
        })
    }

    pub fn list_entries(&self, session_id: &str) -> Result<Vec<Entry>> {
        self.with_conn(|conn| read_entries(conn, session_id, None))
    }

    pub fn append_entry(&self, session_id: &str, kind: EntryKind) -> Result<Entry> {
        self.append_entry_at(session_id, kind, now_ms())
    }

    /// Timestamp-explicit form. The seeder needs it to lay a transcript across past days;
    /// everything else takes `now`.
    pub fn append_entry_at(
        &self,
        session_id: &str,
        kind: EntryKind,
        timestamp: i64,
    ) -> Result<Entry> {
        self.with_tx(|tx| {
            let tip: Option<(i64, String)> = tx
                .query_row(
                    "SELECT seq, id FROM entries WHERE session_id = ?1 ORDER BY seq DESC LIMIT 1",
                    params![session_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            // No tip is ambiguous between "empty session" and "no session", so check.
            if tip.is_none() {
                let exists: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM sessions WHERE id = ?1",
                        params![session_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if exists.is_none() {
                    return Err(CoreError::SessionNotFound(session_id.to_string()));
                }
            }

            let entry = Entry {
                id: new_id("ent"),
                session_id: session_id.to_string(),
                seq: tip.as_ref().map(|(s, _)| *s as u64 + 1).unwrap_or(0),
                parent_id: tip.map(|(_, id)| id),
                timestamp,
                kind,
            };
            write_entry(tx, &entry)?;

            // Tool results are transcript entries but not *messages* — counting them would
            // make the sidebar's message count jump by five for one exchange.
            let is_message = matches!(
                entry.kind,
                EntryKind::Message {
                    message: Message::User(_) | Message::Assistant(_)
                }
            );
            tx.execute(
                "UPDATE sessions SET message_count = message_count + ?2,
                        updated_at = MAX(updated_at, ?3) WHERE id = ?1",
                params![session_id, i64::from(is_message), timestamp],
            )?;
            Ok(entry)
        })
    }

    /// Replace an entry in place, used when a streamed assistant message finalizes.
    pub fn replace_entry(&self, entry: &Entry) -> Result<()> {
        self.with_tx(|tx| {
            let n = tx.execute(
                "UPDATE entries SET kind = ?2, role = ?3, timestamp = ?4, payload = ?5
                 WHERE id = ?1",
                params![
                    entry.id,
                    kind_tag(&entry.kind),
                    role_tag(&entry.kind),
                    entry.timestamp,
                    serde_json::to_string(&entry.kind)?
                ],
            )?;
            if n == 0 {
                return Err(CoreError::InvalidRequest(format!(
                    "entry not found: {}",
                    entry.id
                )));
            }
            index_body(tx, entry)?;
            Ok(())
        })
    }

    /// Drop everything after `entry_id`, which is what `retryMessage` needs before it
    /// re-runs a prompt. The entry itself is kept.
    pub fn truncate_after(&self, session_id: &str, entry_id: &str) -> Result<u64> {
        self.with_tx(|tx| {
            let seq: i64 = tx
                .query_row(
                    "SELECT seq FROM entries WHERE id = ?1 AND session_id = ?2",
                    params![entry_id, session_id],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| CoreError::InvalidRequest(format!("entry not found: {entry_id}")))?;

            let dropped: i64 = tx.query_row(
                "SELECT COUNT(*) FROM entries
                 WHERE session_id = ?1 AND seq > ?2 AND role IN ('user', 'assistant')",
                params![session_id, seq],
                |r| r.get(0),
            )?;
            tx.execute(
                "DELETE FROM search WHERE session_id = ?1 AND entry_id IN
                   (SELECT id FROM entries WHERE session_id = ?1 AND seq > ?2)",
                params![session_id, seq],
            )?;
            tx.execute(
                "DELETE FROM entries WHERE session_id = ?1 AND seq > ?2",
                params![session_id, seq],
            )?;
            tx.execute(
                "UPDATE sessions SET message_count = MAX(0, message_count - ?2),
                        updated_at = ?3 WHERE id = ?1",
                params![session_id, dropped, now_ms()],
            )?;
            Ok(dropped.max(0) as u64)
        })
    }

    /// F1.5 — fork the transcript at `entry_id` into a fresh session, then record where it
    /// came from as a `branch_summary` at the tip.
    pub fn branch_from_message(&self, session_id: &str, entry_id: &str) -> Result<SessionSummary> {
        let now = now_ms();
        let new_session_id = new_id("ses");
        self.with_tx(|tx| {
            let source = summary_by_id(tx, session_id)?;
            let cut: i64 = tx
                .query_row(
                    "SELECT seq FROM entries WHERE id = ?1 AND session_id = ?2",
                    params![entry_id, session_id],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| CoreError::InvalidRequest(format!("entry not found: {entry_id}")))?;

            let branch = SessionSummary {
                id: new_session_id.clone(),
                index: 0,
                status: crate::protocol::SessionStatus::Idle,
                message_count: 0,
                total_tokens: 0,
                archived: false,
                pinned: false,
                created_at: now,
                updated_at: now,
                ..source.clone()
            };
            shift_group_from(tx, branch.group_id.as_deref(), 0, 1)?;
            insert_session(tx, &branch)?;
            index_title(tx, &branch.id, &branch.title)?;

            let copied = read_entries(tx, session_id, Some(cut))?;
            let mut messages = 0i64;
            let mut parent: Option<String> = None;
            for (seq, source_entry) in copied.iter().enumerate() {
                let entry = Entry {
                    id: new_id("ent"),
                    session_id: new_session_id.clone(),
                    seq: seq as u64,
                    parent_id: parent.clone(),
                    timestamp: source_entry.timestamp,
                    kind: source_entry.kind.clone(),
                };
                write_entry(tx, &entry)?;
                if matches!(
                    entry.kind,
                    EntryKind::Message {
                        message: Message::User(_) | Message::Assistant(_)
                    }
                ) {
                    messages += 1;
                }
                parent = Some(entry.id);
            }

            let marker = Entry {
                id: new_id("ent"),
                session_id: new_session_id.clone(),
                seq: copied.len() as u64,
                parent_id: parent,
                timestamp: now,
                kind: EntryKind::BranchSummary {
                    from_id: entry_id.to_string(),
                    summary: format!("Branched from “{}”", source.title),
                },
            };
            write_entry(tx, &marker)?;

            tx.execute(
                "UPDATE sessions SET message_count = ?2 WHERE id = ?1",
                params![new_session_id, messages],
            )?;
            Ok(())
        })?;
        self.get_summary(&new_session_id)
    }
}

// ------------------------------------------------------------ helpers

fn read_entries(conn: &Connection, session_id: &str, max_seq: Option<i64>) -> Result<Vec<Entry>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, seq, parent_id, timestamp, payload FROM entries
         WHERE session_id = ?1 AND (?2 IS NULL OR seq <= ?2) ORDER BY seq ASC",
    )?;
    let rows = stmt
        .query_map(params![session_id, max_seq], |r| {
            let payload: String = r.get(5)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                payload,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, sid, seq, parent_id, timestamp, payload) in rows {
        out.push(Entry {
            id,
            session_id: sid,
            seq: seq.max(0) as u64,
            parent_id,
            timestamp,
            kind: serde_json::from_str(&payload)?,
        });
    }
    Ok(out)
}

pub(in crate::app) fn write_entry(conn: &Connection, entry: &Entry) -> Result<()> {
    conn.execute(
        "INSERT INTO entries (id, session_id, seq, parent_id, kind, role, timestamp, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            entry.id,
            entry.session_id,
            entry.seq as i64,
            entry.parent_id,
            kind_tag(&entry.kind),
            role_tag(&entry.kind),
            entry.timestamp,
            serde_json::to_string(&entry.kind)?,
        ],
    )?;
    index_body(conn, entry)?;
    Ok(())
}

/// Mirror an entry's text into FTS5 (F13.3). Non-message entries carry no prose worth
/// searching, and re-indexing on replace is a delete + insert because fts5 has no upsert.
pub(in crate::app) fn index_body(conn: &Connection, entry: &Entry) -> Result<()> {
    conn.execute(
        "DELETE FROM search WHERE session_id = ?1 AND entry_id = ?2",
        params![entry.session_id, entry.id],
    )?;
    let Some(body) = entry_text(&entry.kind) else {
        return Ok(());
    };
    if body.trim().is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO search (title, body, session_id, entry_id) VALUES ('', ?1, ?2, ?3)",
        params![body, entry.session_id, entry.id],
    )?;
    Ok(())
}

pub(in crate::app) fn entry_text(kind: &EntryKind) -> Option<String> {
    match kind {
        EntryKind::Message { message } => Some(match message {
            Message::User(m) => m.content.to_text(),
            Message::Assistant(m) => m.text(),
            Message::ToolResult(m) => m
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
        }),
        EntryKind::Compaction { summary, .. } | EntryKind::BranchSummary { summary, .. } => {
            Some(summary.clone())
        }
        _ => None,
    }
}

fn kind_tag(kind: &EntryKind) -> &'static str {
    match kind {
        EntryKind::Message { .. } => "message",
        EntryKind::ModelChange { .. } => "model_change",
        EntryKind::ThinkingLevelChange { .. } => "thinking_level_change",
        EntryKind::Compaction { .. } => "compaction",
        EntryKind::BranchSummary { .. } => "branch_summary",
        EntryKind::Custom { .. } => "custom",
    }
}

fn role_tag(kind: &EntryKind) -> Option<&'static str> {
    match kind {
        EntryKind::Message { message } => Some(match message {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        }),
        _ => None,
    }
}
