//! The SQLite-backed session store — the single source of truth for sessions as data.
//!
//! One connection behind a mutex rather than a pool: every write is a short transaction, the
//! only reader is a UI whose queries must finish inside a frame, and WAL means the harness
//! thread and the query thread never block each other for long. A pool would buy contention
//! we do not have and cost us the "one writer, ordered writes" invariant that keeps `idx`
//! renumbering correct.

mod attachments;
mod entries;
mod schema;
mod turns;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row};

pub use attachments::{AddAttachment, AttachmentSource, MAX_ATTACHMENT_BYTES};
pub use turns::{ToolInvocationRecord, TurnRecord};

// Row-level writers the seeder drives directly, so a whole corpus lands in one transaction.
pub(in crate::app) use attachments::{image_dimensions, insert_attachment, sha256_hex};
pub(in crate::app) use entries::write_entry;
pub(in crate::app) use turns::insert_turn;

use crate::error::{CoreError, Result};
use crate::protocol::{
    now_ms, ModelRef, SessionGroup, SessionList, SessionStatus, SessionSummary, ThinkingLevel,
    Workspace,
};

use super::derive_title;

/// How many recent workspace roots are remembered for the folder picker (F4.4).
const MAX_RECENT_ROOTS: usize = 12;

#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// Populate the demo corpus when the database is empty (spec 01 §6).
    pub seed_mock_data: bool,
    /// Fixed RNG seed, so screenshots and tests are stable.
    pub seed: u64,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            seed_mock_data: false,
            seed: super::seed::DEFAULT_SEED,
        }
    }
}

pub struct Store {
    pub(in crate::app) data_dir: PathBuf,
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(data_dir: &Path) -> Result<Self> {
        Self::open_with(data_dir, StoreOptions::default())
    }

    pub fn open_with(data_dir: &Path, options: StoreOptions) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        std::fs::create_dir_all(data_dir.join("attachments"))?;

        let conn = Connection::open(data_dir.join("form.sqlite"))?;
        // journal_mode returns a row, so it cannot go through `pragma_update`.
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        schema::migrate(&conn)?;

        // A crash mid-run leaves `streaming` behind; the run is gone, so the row is not.
        conn.execute(
            "UPDATE sessions SET status = 'idle' WHERE status = 'streaming'",
            [],
        )?;

        let store = Self {
            data_dir: data_dir.to_path_buf(),
            conn: Mutex::new(conn),
        };

        if options.seed_mock_data && store.is_empty()? {
            super::seed::seed(&store, options.seed, now_ms())?;
        }
        Ok(store)
    }

    pub fn schema_version(&self) -> Result<u32> {
        self.with_conn(schema::schema_version)
    }

    /// True when no session has ever been created. Seeding keys off this, not off a flag
    /// file, so a user who deletes every session does not get the demo corpus back.
    pub fn is_empty(&self) -> Result<bool> {
        self.with_conn(|conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
            Ok(n == 0)
        })
    }

    // ------------------------------------------------------------ plumbing

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A poisoned mutex means a panic inside a transaction; the transaction rolled back,
        // so the connection is still usable and refusing every later call helps nobody.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(in crate::app) fn with_conn<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        let guard = self.lock();
        f(&guard)
    }

    /// Run `f` inside a transaction, committing on `Ok` and rolling back on `Err`.
    pub(in crate::app) fn with_tx<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut guard = self.lock();
        let tx = guard.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ------------------------------------------------------------ sessions

    pub fn list_sessions(&self, include_archived: bool) -> Result<SessionList> {
        self.with_conn(|conn| {
            let groups = read_groups(conn)?;
            // Grouped sessions first in group order, then ungrouped — a stable order the
            // sidebar can render without re-sorting.
            let mut stmt = conn.prepare(
                "SELECT s.* FROM sessions s
                 LEFT JOIN groups g ON g.id = s.group_id
                 WHERE (?1 OR s.archived = 0)
                 ORDER BY (s.group_id IS NULL) ASC, g.idx ASC, s.idx ASC",
            )?;
            let sessions = stmt
                .query_map(params![include_archived], |row| Ok(read_summary(row)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(SessionList { groups, sessions })
        })
    }

    pub fn get_summary(&self, id: &str) -> Result<SessionSummary> {
        self.with_conn(|conn| summary_by_id(conn, id))
    }

    pub fn create_session(
        &self,
        group_id: Option<String>,
        title: Option<String>,
        workspace_root: Option<String>,
        model_ref: Option<ModelRef>,
    ) -> Result<SessionSummary> {
        let now = now_ms();
        let summary = SessionSummary {
            id: new_id("ses"),
            title: title.clone().unwrap_or_else(|| super::UNTITLED.to_string()),
            title_is_custom: title.is_some(),
            group_id,
            index: 0,
            workspace_root,
            model_ref: model_ref.unwrap_or_else(super::default_model_ref),
            status: SessionStatus::Idle,
            message_count: 0,
            total_tokens: 0,
            archived: false,
            pinned: false,
            created_at: now,
            updated_at: now,
        };
        self.with_tx(|tx| {
            if let Some(gid) = &summary.group_id {
                require_group(tx, gid)?;
            }
            // New chats belong at the top of their group (F2.1, newest-first).
            shift_group_from(tx, summary.group_id.as_deref(), 0, 1)?;
            insert_session(tx, &summary)?;
            index_title(tx, &summary.id, &summary.title)?;
            if let Some(root) = &summary.workspace_root {
                touch_root(tx, root, now)?;
            }
            Ok(())
        })?;
        self.get_summary(&summary.id)
    }

    /// F2.6 — auto-derive from the first user message until the user renames.
    pub fn maybe_derive_title(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<Option<SessionSummary>> {
        let derived = derive_title(text);
        if derived.is_empty() {
            return Ok(None);
        }
        let changed = self.with_tx(|tx| {
            let custom: bool = tx
                .query_row(
                    "SELECT title_is_custom FROM sessions WHERE id = ?1",
                    params![session_id],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| CoreError::SessionNotFound(session_id.to_string()))?;
            if custom {
                return Ok(false);
            }
            tx.execute(
                "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1",
                params![session_id, derived, now_ms()],
            )?;
            index_title(tx, session_id, &derived)?;
            Ok(true)
        })?;
        if changed {
            Ok(Some(self.get_summary(session_id)?))
        } else {
            Ok(None)
        }
    }

    /// A manual rename pins the title (F2.6) — auto-derivation never overwrites it again.
    pub fn rename_session(&self, session_id: &str, title: &str) -> Result<SessionSummary> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(CoreError::InvalidRequest("title is empty".to_string()));
        }
        self.with_tx(|tx| {
            let n = tx.execute(
                "UPDATE sessions SET title = ?2, title_is_custom = 1, updated_at = ?3
                 WHERE id = ?1",
                params![session_id, title, now_ms()],
            )?;
            if n == 0 {
                return Err(CoreError::SessionNotFound(session_id.to_string()));
            }
            index_title(tx, session_id, &title)?;
            Ok(())
        })?;
        self.get_summary(session_id)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.with_tx(|tx| {
            let (group_id, idx) = session_slot(tx, session_id)?;
            // Entries and turns cascade; the FTS mirror has no foreign key, so it does not.
            tx.execute(
                "DELETE FROM search WHERE session_id = ?1",
                params![session_id],
            )?;
            tx.execute(
                "DELETE FROM tool_invocations WHERE session_id = ?1",
                params![session_id],
            )?;
            tx.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
            shift_group_from(tx, group_id.as_deref(), idx + 1, -1)?;
            Ok(())
        })
    }

    pub fn set_archived(&self, session_id: &str, archived: bool) -> Result<SessionSummary> {
        self.set_flag(session_id, "archived", archived)
    }

    pub fn set_pinned(&self, session_id: &str, pinned: bool) -> Result<SessionSummary> {
        self.set_flag(session_id, "pinned", pinned)
    }

    fn set_flag(&self, session_id: &str, column: &str, value: bool) -> Result<SessionSummary> {
        // `column` is never caller-supplied — the two call sites above pass literals.
        let sql = format!("UPDATE sessions SET {column} = ?2, updated_at = ?3 WHERE id = ?1");
        self.with_conn(|conn| {
            let n = conn.execute(&sql, params![session_id, value, now_ms()])?;
            if n == 0 {
                return Err(CoreError::SessionNotFound(session_id.to_string()));
            }
            Ok(())
        })?;
        self.get_summary(session_id)
    }

    pub fn set_status(&self, session_id: &str, status: SessionStatus) -> Result<SessionSummary> {
        self.with_conn(|conn| {
            let n = conn.execute(
                "UPDATE sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![session_id, status_str(status), now_ms()],
            )?;
            if n == 0 {
                return Err(CoreError::SessionNotFound(session_id.to_string()));
            }
            Ok(())
        })?;
        self.get_summary(session_id)
    }

    pub fn add_tokens(&self, session_id: &str, tokens: u64) -> Result<SessionSummary> {
        self.with_conn(|conn| {
            let n = conn.execute(
                "UPDATE sessions SET total_tokens = total_tokens + ?2 WHERE id = ?1",
                params![session_id, tokens as i64],
            )?;
            if n == 0 {
                return Err(CoreError::SessionNotFound(session_id.to_string()));
            }
            Ok(())
        })?;
        self.get_summary(session_id)
    }

    pub fn set_session_model(
        &self,
        session_id: &str,
        model_ref: &ModelRef,
    ) -> Result<SessionSummary> {
        self.with_conn(|conn| {
            let n = conn.execute(
                "UPDATE sessions SET provider_id = ?2, model_id = ?3, thinking_level = ?4,
                        updated_at = ?5 WHERE id = ?1",
                params![
                    session_id,
                    model_ref.provider_id,
                    model_ref.model_id,
                    model_ref.thinking_level.as_str(),
                    now_ms()
                ],
            )?;
            if n == 0 {
                return Err(CoreError::SessionNotFound(session_id.to_string()));
            }
            Ok(())
        })?;
        self.get_summary(session_id)
    }

    /// `None` makes the session explicitly unconfined (F4.5).
    pub fn set_workspace_root(
        &self,
        session_id: &str,
        path: Option<String>,
    ) -> Result<SessionSummary> {
        let now = now_ms();
        self.with_tx(|tx| {
            let n = tx.execute(
                "UPDATE sessions SET workspace_root = ?2, updated_at = ?3 WHERE id = ?1",
                params![session_id, path, now],
            )?;
            if n == 0 {
                return Err(CoreError::SessionNotFound(session_id.to_string()));
            }
            if let Some(root) = &path {
                touch_root(tx, root, now)?;
            }
            Ok(())
        })?;
        self.get_summary(session_id)
    }

    /// Dense reordering (spec 01 §4). Removing from the old slot and inserting into the new
    /// one happen in one transaction so `idx` is never observably sparse or duplicated.
    pub fn move_session(&self, id: &str, group: Option<&str>, index: u32) -> Result<()> {
        self.with_tx(|tx| {
            if let Some(gid) = group {
                require_group(tx, gid)?;
            }
            let (from_group, from_idx) = session_slot(tx, id)?;
            shift_group_from(tx, from_group.as_deref(), from_idx + 1, -1)?;

            let occupied: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sessions WHERE id <> ?1 AND group_id IS ?2",
                params![id, group],
                |r| r.get(0),
            )?;
            let target = (index as i64).clamp(0, occupied);
            shift_group_from(tx, group, target, 1)?;
            tx.execute(
                "UPDATE sessions SET group_id = ?2, idx = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, group, target, now_ms()],
            )?;
            Ok(())
        })
    }

    // ------------------------------------------------------------ groups

    pub fn list_groups(&self) -> Result<Vec<SessionGroup>> {
        self.with_conn(read_groups)
    }

    pub fn create_group(&self, name: &str) -> Result<SessionGroup> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(CoreError::InvalidRequest("group name is empty".to_string()));
        }
        let id = new_id("grp");
        self.with_conn(|conn| {
            let idx: i64 = conn.query_row("SELECT COUNT(*) FROM groups", [], |r| r.get(0))?;
            conn.execute(
                "INSERT INTO groups (id, name, idx, collapsed, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4)",
                params![id, name, idx, now_ms()],
            )?;
            Ok(SessionGroup {
                id: id.clone(),
                name,
                index: idx as u32,
                collapsed: false,
            })
        })
    }

    pub fn rename_group(&self, group_id: &str, name: &str) -> Result<Vec<SessionGroup>> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(CoreError::InvalidRequest("group name is empty".to_string()));
        }
        self.with_conn(|conn| {
            let n = conn.execute(
                "UPDATE groups SET name = ?2 WHERE id = ?1",
                params![group_id, name],
            )?;
            if n == 0 {
                return Err(CoreError::GroupNotFound(group_id.to_string()));
            }
            read_groups(conn)
        })
    }

    pub fn set_group_collapsed(
        &self,
        group_id: &str,
        collapsed: bool,
    ) -> Result<Vec<SessionGroup>> {
        self.with_conn(|conn| {
            let n = conn.execute(
                "UPDATE groups SET collapsed = ?2 WHERE id = ?1",
                params![group_id, collapsed],
            )?;
            if n == 0 {
                return Err(CoreError::GroupNotFound(group_id.to_string()));
            }
            read_groups(conn)
        })
    }

    /// Deleting a group does not delete its sessions — they fall back to `Ungrouped`, which
    /// is the only non-destructive reading of "delete group" in the sidebar (F2.2).
    pub fn delete_group(&self, group_id: &str) -> Result<Vec<SessionGroup>> {
        self.with_tx(|tx| {
            let idx: Option<i64> = tx
                .query_row(
                    "SELECT idx FROM groups WHERE id = ?1",
                    params![group_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(idx) = idx else {
                return Err(CoreError::GroupNotFound(group_id.to_string()));
            };

            let orphans: Vec<String> = {
                let mut stmt =
                    tx.prepare("SELECT id FROM sessions WHERE group_id = ?1 ORDER BY idx ASC")?;
                let rows = stmt
                    .query_map(params![group_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                rows
            };
            // Append them to the ungrouped run, preserving their relative order.
            let mut next: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sessions WHERE group_id IS NULL",
                [],
                |r| r.get(0),
            )?;
            for id in orphans {
                tx.execute(
                    "UPDATE sessions SET group_id = NULL, idx = ?2 WHERE id = ?1",
                    params![id, next],
                )?;
                next += 1;
            }

            tx.execute("DELETE FROM groups WHERE id = ?1", params![group_id])?;
            tx.execute(
                "UPDATE groups SET idx = idx - 1 WHERE idx > ?1",
                params![idx],
            )?;
            read_groups(tx)
        })
    }

    pub fn reorder_group(&self, group_id: &str, index: u32) -> Result<Vec<SessionGroup>> {
        self.with_tx(|tx| {
            let from: Option<i64> = tx
                .query_row(
                    "SELECT idx FROM groups WHERE id = ?1",
                    params![group_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(from) = from else {
                return Err(CoreError::GroupNotFound(group_id.to_string()));
            };
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM groups", [], |r| r.get(0))?;
            let to = (index as i64).clamp(0, (count - 1).max(0));
            if to != from {
                tx.execute(
                    "UPDATE groups SET idx = -1 WHERE id = ?1",
                    params![group_id],
                )?;
                if to < from {
                    tx.execute(
                        "UPDATE groups SET idx = idx + 1 WHERE idx >= ?1 AND idx < ?2",
                        params![to, from],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE groups SET idx = idx - 1 WHERE idx > ?1 AND idx <= ?2",
                        params![from, to],
                    )?;
                }
                tx.execute(
                    "UPDATE groups SET idx = ?2 WHERE id = ?1",
                    params![group_id, to],
                )?;
            }
            read_groups(tx)
        })
    }

    // ------------------------------------------------------------ recent roots

    pub fn list_recent_roots(&self) -> Result<Vec<Workspace>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT path, last_used FROM recent_roots ORDER BY last_used DESC LIMIT ?1",
            )?;
            let rows = stmt
                .query_map(params![MAX_RECENT_ROOTS as i64], |r| {
                    Ok(Workspace {
                        path: r.get(0)?,
                        last_used: r.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn touch_recent_root(&self, path: &str) -> Result<()> {
        self.with_conn(|conn| touch_root(conn, path, now_ms()))
    }
}

// ------------------------------------------------------------ row helpers

pub(in crate::app) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn read_groups(conn: &Connection) -> Result<Vec<SessionGroup>> {
    let mut stmt = conn.prepare("SELECT id, name, idx, collapsed FROM groups ORDER BY idx ASC")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SessionGroup {
                id: r.get(0)?,
                name: r.get(1)?,
                index: r.get::<_, i64>(2)? as u32,
                collapsed: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn summary_by_id(conn: &Connection, id: &str) -> Result<SessionSummary> {
    conn.query_row("SELECT * FROM sessions WHERE id = ?1", params![id], |row| {
        Ok(read_summary(row))
    })
    .optional()?
    .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))
}

/// Reads by column name so `SELECT *` stays valid as later migrations append columns.
fn read_summary(row: &Row<'_>) -> SessionSummary {
    let get_str = |name: &str| row.get::<_, String>(name).unwrap_or_default();
    SessionSummary {
        id: get_str("id"),
        title: get_str("title"),
        title_is_custom: row.get("title_is_custom").unwrap_or(false),
        group_id: row.get("group_id").unwrap_or(None),
        index: row.get::<_, i64>("idx").unwrap_or(0) as u32,
        workspace_root: row.get("workspace_root").unwrap_or(None),
        model_ref: ModelRef {
            provider_id: get_str("provider_id"),
            model_id: get_str("model_id"),
            thinking_level: parse_thinking(&get_str("thinking_level")),
        },
        status: parse_status(&get_str("status")),
        message_count: row.get::<_, i64>("message_count").unwrap_or(0).max(0) as u64,
        total_tokens: row.get::<_, i64>("total_tokens").unwrap_or(0).max(0) as u64,
        archived: row.get("archived").unwrap_or(false),
        pinned: row.get("pinned").unwrap_or(false),
        created_at: row.get("created_at").unwrap_or(0),
        updated_at: row.get("updated_at").unwrap_or(0),
    }
}

pub(in crate::app) fn insert_session(conn: &Connection, s: &SessionSummary) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, title, title_is_custom, group_id, idx, workspace_root,
                               provider_id, model_id, thinking_level, status, archived, pinned,
                               message_count, total_tokens, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            s.id,
            s.title,
            s.title_is_custom,
            s.group_id,
            s.index as i64,
            s.workspace_root,
            s.model_ref.provider_id,
            s.model_ref.model_id,
            s.model_ref.thinking_level.as_str(),
            status_str(s.status),
            s.archived,
            s.pinned,
            s.message_count as i64,
            s.total_tokens as i64,
            s.created_at,
            s.updated_at,
        ],
    )?;
    Ok(())
}

fn session_slot(conn: &Connection, id: &str) -> Result<(Option<String>, i64)> {
    conn.query_row(
        "SELECT group_id, idx FROM sessions WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))
}

fn require_group(conn: &Connection, group_id: &str) -> Result<()> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM groups WHERE id = ?1",
            params![group_id],
            |r| r.get(0),
        )
        .optional()?;
    exists
        .map(|_| ())
        .ok_or_else(|| CoreError::GroupNotFound(group_id.to_string()))
}

/// Shift every session at or after `from` in `group` by `delta`, keeping `idx` dense.
fn shift_group_from(conn: &Connection, group: Option<&str>, from: i64, delta: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET idx = idx + ?3 WHERE group_id IS ?1 AND idx >= ?2",
        params![group, from, delta],
    )?;
    Ok(())
}

pub(in crate::app) fn touch_root(conn: &Connection, path: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO recent_roots (path, last_used) VALUES (?1, ?2)
         ON CONFLICT(path) DO UPDATE SET last_used = excluded.last_used",
        params![path, now],
    )?;
    conn.execute(
        "DELETE FROM recent_roots WHERE path NOT IN
           (SELECT path FROM recent_roots ORDER BY last_used DESC LIMIT ?1)",
        params![MAX_RECENT_ROOTS as i64],
    )?;
    Ok(())
}

/// The FTS mirror keeps exactly one title row per session, marked by an empty `entry_id`.
pub(in crate::app) fn index_title(conn: &Connection, session_id: &str, title: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM search WHERE session_id = ?1 AND entry_id = ''",
        params![session_id],
    )?;
    conn.execute(
        "INSERT INTO search (title, body, session_id, entry_id) VALUES (?1, '', ?2, '')",
        params![title, session_id],
    )?;
    Ok(())
}

fn status_str(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Streaming => "streaming",
        SessionStatus::Error => "error",
    }
}

fn parse_status(raw: &str) -> SessionStatus {
    match raw {
        "streaming" => SessionStatus::Streaming,
        "error" => SessionStatus::Error,
        _ => SessionStatus::Idle,
    }
}

fn parse_thinking(raw: &str) -> ThinkingLevel {
    match raw {
        "off" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => ThinkingLevel::High,
    }
}
