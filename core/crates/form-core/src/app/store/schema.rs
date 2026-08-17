//! Schema migrations.
//!
//! `MIGRATIONS[i]` takes the database from version `i` to version `i + 1`. Appending is the
//! only legal change: rewriting an existing migration would silently diverge two machines
//! that upgraded at different times.

use rusqlite::Connection;

use crate::error::Result;

type Migration = fn(&Connection) -> Result<()>;

const MIGRATIONS: &[Migration] = &[v1, v2];

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

/// v2 — index the per-day message aggregate.
///
/// The dashboard's "messages per day" is the one `GROUP BY` with nothing behind it, so
/// SQLite scanned `entries` and touched every row's `payload` blob to reach `timestamp`.
/// Leading on `kind` lets the `kind = 'message'` filter seek, and carrying `timestamp` makes
/// the range an index-only scan whose cost tracks row count rather than transcript size.
fn v2(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE INDEX entries_kind_timestamp ON entries(kind, timestamp);")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The upgrade path is the one migrations exist for: a database created before v2 must
    /// end up identical to one created fresh.
    #[test]
    fn an_existing_v1_database_gains_the_v2_index() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        v1(&conn).unwrap();
        set_schema_version(&conn, 1).unwrap();
        assert!(!has_index(&conn, "entries_kind_timestamp"));

        migrate(&conn).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as u32);
        assert!(has_index(&conn, "entries_kind_timestamp"));
    }

    /// The point of the index: the per-day message count must not have to read `payload`.
    #[test]
    fn the_daily_message_aggregate_is_index_only() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN
                 SELECT timestamp / 86400000, COUNT(*) FROM entries
                 WHERE kind = 'message' AND timestamp >= 0 GROUP BY 1",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(
            plan.contains("entries_kind_timestamp"),
            "expected the new index in the plan, got: {plan}"
        );
        assert!(
            plan.contains("COVERING INDEX"),
            "expected an index-only scan, got: {plan}"
        );
    }

    fn has_index(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

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
