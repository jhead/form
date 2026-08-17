//! Schema migrations.
//!
//! `MIGRATIONS[i]` takes the database from version `i` to version `i + 1`. Appending is the
//! only legal change: rewriting an existing migration would silently diverge two machines
//! that upgraded at different times.

use rusqlite::Connection;

use crate::error::Result;

type Migration = fn(&Connection) -> Result<()>;

const MIGRATIONS: &[Migration] = &[v1];

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;

    let current = schema_version(conn)?;
    for (i, migration) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        migration(conn)?;
        set_schema_version(conn, i as u32 + 1)?;
    }
    Ok(())
}

pub fn schema_version(conn: &Connection) -> Result<u32> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    Ok(raw.and_then(|v| v.parse().ok()).unwrap_or(0))
}

fn set_schema_version(conn: &Connection, version: u32) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [version.to_string()],
    )?;
    Ok(())
}

/// v1 — the initial create. See `docs/specs/01-core-domain.md` §2.
///
/// `sessions.message_count` / `sessions.total_tokens` are denormalized counters the spec's
/// schema sketch omits but `SessionSummary` requires; deriving them per row would put two
/// correlated subqueries on the sidebar's hot path.
fn v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE groups (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  idx INTEGER NOT NULL,
  collapsed INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  title_is_custom INTEGER NOT NULL DEFAULT 0,
  group_id TEXT REFERENCES groups(id) ON DELETE SET NULL,
  idx INTEGER NOT NULL,
  workspace_root TEXT,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  thinking_level TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'idle',
  archived INTEGER NOT NULL DEFAULT 0,
  pinned INTEGER NOT NULL DEFAULT 0,
  message_count INTEGER NOT NULL DEFAULT 0,
  total_tokens INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX sessions_group_idx ON sessions(group_id, idx);
CREATE INDEX sessions_updated ON sessions(updated_at DESC);

CREATE TABLE entries (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  parent_id TEXT,
  kind TEXT NOT NULL,
  role TEXT,
  timestamp INTEGER NOT NULL,
  payload TEXT NOT NULL
);
CREATE UNIQUE INDEX entries_seq ON entries(session_id, seq);

CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  thinking_level TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER NOT NULL,
  ttft_ms INTEGER,
  duration_ms INTEGER NOT NULL,
  input INTEGER NOT NULL,
  output INTEGER NOT NULL,
  cache_read INTEGER NOT NULL,
  cache_write INTEGER NOT NULL,
  reasoning INTEGER,
  total_tokens INTEGER NOT NULL,
  cost_total REAL NOT NULL,
  outcome TEXT NOT NULL
);
CREATE INDEX turns_started ON turns(started_at);
CREATE INDEX turns_session ON turns(session_id);

CREATE TABLE tool_invocations (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  tool_name TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  duration_ms INTEGER NOT NULL,
  is_error INTEGER NOT NULL
);
CREATE INDEX tool_invocations_turn ON tool_invocations(turn_id);
CREATE INDEX tool_invocations_started ON tool_invocations(started_at);

CREATE TABLE attachments (
  id TEXT PRIMARY KEY,
  session_id TEXT,
  sha256 TEXT NOT NULL,
  filename TEXT NOT NULL,
  mime TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  width INTEGER,
  height INTEGER,
  path TEXT NOT NULL,
  thumb_path TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX attachments_sha ON attachments(sha256);
CREATE INDEX attachments_session ON attachments(session_id);

CREATE TABLE recent_roots (path TEXT PRIMARY KEY, last_used INTEGER NOT NULL);

CREATE VIRTUAL TABLE search USING fts5(
  title, body, session_id UNINDEXED, entry_id UNINDEXED, tokenize='porter unicode61'
);
"#,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_from_empty_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 0);

        migrate(&conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as u32);

        // Re-running must be a no-op rather than a "table already exists" failure.
        migrate(&conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as u32);

        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type IN ('table') ORDER BY name")
                .unwrap();
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows
        };
        for expected in [
            "attachments",
            "entries",
            "groups",
            "meta",
            "recent_roots",
            "search",
            "sessions",
            "tool_invocations",
            "turns",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected} in {tables:?}"
            );
        }
    }
}
