//! FTS5 search over a co-located session database. Port of
//! `sqlite/search-backend.ts`.
//!
//! The index is an *external content* FTS5 table over `entries.payload` with a
//! trigram tokenizer, so substring matches work without a word tokenizer.
//! Because it is external-content, the index is only correct while its triggers
//! are installed — which is why a dropped `session_search_fts` makes canonical
//! writes fail rather than silently desync (upstream tests that explicitly).
//!
//! The schema is created lazily, on the first non-blank search: a repository
//! that is never searched never pays for the index, and never installs the
//! triggers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex as AsyncMutex;

use pi_session::repo::{throw_if_aborted, SearchBackend, SessionSearchHit, SessionSearchOptions};
use pi_session::{SessionError, SessionResult};

use crate::repo::{absolute_path, create_parent_directory, open_configured_connection};
use crate::sql::{transaction, SqlQuery};

/// The FTS5 table and the triggers that keep it in step with `entries`.
const SEARCH_SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS session_search_fts USING fts5(
  payload,
  content = 'entries',
  content_rowid = 'rowid',
  tokenize = 'trigram remove_diacritics 1'
);
CREATE TRIGGER IF NOT EXISTS session_search_fts_ai AFTER INSERT ON entries BEGIN
  INSERT INTO session_search_fts(rowid, payload) VALUES (new.rowid, new.payload);
END;
CREATE TRIGGER IF NOT EXISTS session_search_fts_ad AFTER DELETE ON entries BEGIN
  INSERT INTO session_search_fts(session_search_fts, rowid, payload) VALUES('delete', old.rowid, old.payload);
END;
CREATE TRIGGER IF NOT EXISTS session_search_fts_au AFTER UPDATE OF payload ON entries BEGIN
  INSERT INTO session_search_fts(session_search_fts, rowid, payload) VALUES('delete', old.rowid, old.payload);
  INSERT INTO session_search_fts(rowid, payload) VALUES (new.rowid, new.payload);
END;
";

fn table_exists(conn: &Connection, name: &str) -> SessionResult<bool> {
    let mut query =
        SqlQuery::raw("SELECT 1 AS found FROM sqlite_master WHERE type = 'table' AND name = ");
    query.bind(name).push(" LIMIT 1");
    query.exists(conn)
}

fn rebuild_search_index(conn: &Connection) -> SessionResult<()> {
    SqlQuery::raw("INSERT INTO session_search_fts(session_search_fts) VALUES('rebuild')")
        .run(conn)
        .map(|_| ())
}

fn ensure_search_schema(conn: &Connection) -> SessionResult<()> {
    let fts_exists = table_exists(conn, "session_search_fts")?;
    let entries_exist = table_exists(conn, "entries")?;
    transaction(conn, |conn| {
        SqlQuery::raw(SEARCH_SCHEMA).exec(conn)?;
        // A database that already had entries before the index existed needs a
        // one-off backfill; the triggers only cover writes from here on.
        if !fts_exists && entries_exist {
            rebuild_search_index(conn)?;
        }
        Ok(())
    })
}

/// SQLite FTS search over a canonical session database.
///
/// Upstream opens and closes a connection per search; this keeps one lazily
/// opened connection instead, because a Rust caller cannot rely on the search
/// object being disposed. [`SqliteSearchBackend::close`] drops it explicitly.
pub struct SqliteSearchBackend {
    database_path: PathBuf,
    connection: AsyncMutex<Option<Arc<AsyncMutex<Connection>>>>,
}

impl std::fmt::Debug for SqliteSearchBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSearchBackend")
            .field("database_path", &self.database_path)
            .finish()
    }
}

impl SqliteSearchBackend {
    pub fn new(database_path: impl Into<PathBuf>) -> Self {
        Self {
            database_path: database_path.into(),
            connection: AsyncMutex::new(None),
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Drops the search connection. Idempotent.
    pub async fn close(&self) {
        *self.connection.lock().await = None;
    }

    async fn database(&self) -> SessionResult<Arc<AsyncMutex<Connection>>> {
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_ref() {
            return Ok(connection.clone());
        }
        let path = absolute_path(&self.database_path)?;
        create_parent_directory(&path).await?;
        let conn = tokio::task::spawn_blocking(move || {
            let conn = open_configured_connection(&path)?;
            match ensure_search_schema(&conn) {
                Ok(()) => Ok(conn),
                Err(error) => {
                    let _ = conn.close();
                    Err(error)
                }
            }
        })
        .await
        .map_err(|error| SessionError::storage(format!("SQLite worker task failed: {error}")))??;
        let connection = Arc::new(AsyncMutex::new(conn));
        *slot = Some(connection.clone());
        Ok(connection)
    }
}

#[async_trait]
impl SearchBackend for SqliteSearchBackend {
    async fn search(
        &self,
        text: &str,
        options: &SessionSearchOptions,
    ) -> SessionResult<Vec<SessionSearchHit>> {
        let query_text = text.trim().to_string();
        if query_text.is_empty() || options.limit.is_some_and(|limit| limit <= 0) {
            return Ok(Vec::new());
        }
        if options
            .entry_types
            .as_ref()
            .is_some_and(|types| types.is_empty())
        {
            return Ok(Vec::new());
        }
        throw_if_aborted(options.signal.as_ref())?;

        let entry_types: Option<Vec<String>> = options.entry_types.as_ref().map(|types| {
            types
                .iter()
                .map(|entry_type| entry_type.as_str().to_string())
                .collect()
        });
        let limit = options.limit.unwrap_or(-1);
        let db = self.database().await?;
        let guard = db.lock_owned().await;
        let hits = tokio::task::spawn_blocking(move || {
            let conn: &Connection = &guard;
            // The whole trimmed input becomes one quoted FTS phrase, so user
            // text can never inject FTS operators.
            let phrase = format!("\"{}\"", query_text.replace('"', "\"\""));
            let mut query = SqlQuery::raw(
                "SELECT s.id, se.id AS entry_id, se.timestamp, bm25(session_search_fts) AS score
\t\t\t\t\tFROM session_search_fts
\t\t\t\t\tJOIN entries AS se ON se.rowid = session_search_fts.rowid
\t\t\t\t\tJOIN sessions AS s ON s.id = se.session_id
\t\t\t\t\tWHERE session_search_fts MATCH ",
            );
            query.bind(phrase);
            if let Some(entry_types) = &entry_types {
                query.push(" AND se.type IN (");
                for (index, entry_type) in entry_types.iter().enumerate() {
                    if index > 0 {
                        query.push(", ");
                    }
                    query.bind(entry_type.as_str());
                }
                query.push(")");
            }
            query
                .push("\n\t\t\t\t\tORDER BY score\n\t\t\t\t\tLIMIT ")
                .bind(limit);
            query.all(conn, |row| {
                Ok(SessionSearchHit {
                    session_id: row.get("id")?,
                    entry_id: row.get("entry_id")?,
                    timestamp: Some(row.get("timestamp")?),
                    snippet: None,
                    score: Some(row.get("score")?),
                })
            })
        })
        .await
        .map_err(|error| SessionError::storage(format!("SQLite worker task failed: {error}")))??;

        throw_if_aborted(options.signal.as_ref())?;
        Ok(hits)
    }
}
