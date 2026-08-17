//! Schema migrations. Port of `sqlite/migrations.ts`.
//!
//! The migration list, its ids and the `migrations` bookkeeping table match
//! upstream exactly: a database written by the TypeScript backend opens here
//! without re-running anything, and vice versa. `001_initial.sql` is a
//! byte-for-byte copy of the upstream file.

use std::collections::HashSet;

use rusqlite::Connection;

use pi_session::SessionResult;

use crate::sql::{transaction, SqlQuery};

/// One numbered migration.
#[derive(Debug, Clone, Copy)]
pub struct SqliteMigration {
    pub id: &'static str,
    pub order: u32,
    pub sql: &'static str,
}

/// Every migration in application order.
pub fn load_migrations() -> Vec<SqliteMigration> {
    vec![SqliteMigration {
        id: "001_initial.sql",
        order: 1,
        sql: include_str!("migrations/001_initial.sql"),
    }]
}

fn ensure_migrations_table(conn: &Connection) -> SessionResult<()> {
    SqlQuery::raw(
        "
CREATE TABLE IF NOT EXISTS migrations (
\tid TEXT PRIMARY KEY,
\tapplied_at TEXT NOT NULL
);
",
    )
    .exec(conn)
}

/// Applies every migration that this database has not already recorded.
pub fn apply_migrations(conn: &Connection) -> SessionResult<()> {
    ensure_migrations_table(conn)?;
    let applied: HashSet<String> =
        SqlQuery::raw("SELECT id FROM migrations ORDER BY applied_at, id")
            .all(conn, |row| row.get::<_, String>(0))?
            .into_iter()
            .collect();

    for migration in load_migrations() {
        if applied.contains(migration.id) {
            continue;
        }
        transaction(conn, |conn| {
            SqlQuery::raw(migration.sql).exec(conn)?;
            let mut insert = SqlQuery::raw("INSERT INTO migrations (id, applied_at) VALUES (");
            insert
                .bind(migration.id)
                .push(", ")
                .bind(now_iso8601())
                .push(")");
            insert.run(conn)?;
            Ok(())
        })?;
    }
    Ok(())
}

/// `new Date().toISOString()`.
fn now_iso8601() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of `migrations.test.ts`.
    #[test]
    fn applies_the_current_schema_once_and_records_its_migration() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn).unwrap();
        apply_migrations(&conn).unwrap();

        let ids: Vec<String> = SqlQuery::raw("SELECT id FROM migrations ORDER BY id")
            .all(&conn, |row| row.get(0))
            .unwrap();
        assert_eq!(ids, vec!["001_initial.sql".to_string()]);

        let tables: Vec<String> =
            SqlQuery::raw("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .all(&conn, |row| row.get(0))
                .unwrap();
        for expected in [
            "migrations",
            "sessions",
            "entries",
            "session_sequences",
            "session_stats",
            "branch_entries",
            "branch_tips",
            "lanes",
            "records",
            "lane_moves",
            "facts",
            "writer_leases",
        ] {
            assert!(
                tables.iter().any(|name| name == expected),
                "missing table {expected} in {tables:?}"
            );
        }

        let session_columns: Vec<String> = SqlQuery::raw("PRAGMA table_info(sessions)")
            .all(&conn, |row| row.get("name"))
            .unwrap();
        assert!(!session_columns.iter().any(|name| name == "leaf_id"));

        let session_indexes: Vec<String> = SqlQuery::raw("PRAGMA index_list(sessions)")
            .all(&conn, |row| row.get("name"))
            .unwrap();
        assert!(session_indexes
            .iter()
            .any(|name| name == "idx_sessions_cwd_created_at"));
        assert!(!session_indexes
            .iter()
            .any(|name| name == "idx_sessions_parent"));

        let lane_columns: Vec<String> = SqlQuery::raw("PRAGMA table_info(lanes)")
            .all(&conn, |row| row.get("name"))
            .unwrap();
        assert!(lane_columns.iter().any(|name| name == "open_operation_id"));

        let entry_indexes: Vec<String> = SqlQuery::raw("PRAGMA index_list(entries)")
            .all(&conn, |row| row.get("name"))
            .unwrap();
        assert!(!entry_indexes
            .iter()
            .any(|name| name == "idx_entries_session_seq"));

        let branch_entry_indexes: Vec<String> = SqlQuery::raw("PRAGMA index_list(branch_entries)")
            .all(&conn, |row| row.get("name"))
            .unwrap();
        assert!(branch_entry_indexes
            .iter()
            .any(|name| name == "idx_branch_entries_session_entry"));

        let record_indexes: Vec<String> = SqlQuery::raw("PRAGMA index_list(records)")
            .all(&conn, |row| row.get("name"))
            .unwrap();
        for expected in [
            "idx_records_session_lane_seq",
            "idx_records_session_type_seq",
            "idx_records_session_type_op_kind_seq",
        ] {
            assert!(record_indexes.iter().any(|name| name == expected));
        }
        assert!(!record_indexes
            .iter()
            .any(|name| name == "idx_records_session_seq"));

        let lane_move_indexes: Vec<String> = SqlQuery::raw("PRAGMA index_list(lane_moves)")
            .all(&conn, |row| row.get("name"))
            .unwrap();
        assert!(!lane_move_indexes
            .iter()
            .any(|name| name == "idx_lane_moves_session_lane_seq"));
    }

    /// `entries` must keep its implicit rowid: the FTS5 index is an external
    /// content table keyed on it.
    #[test]
    fn entries_keep_a_rowid_for_the_search_index() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn).unwrap();
        SqlQuery::raw("SELECT rowid FROM entries LIMIT 1")
            .all(&conn, |_| Ok(()))
            .expect("entries must expose a rowid");
    }
}
