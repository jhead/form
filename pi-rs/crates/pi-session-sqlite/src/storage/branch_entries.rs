//! `branch_entries` access. Port of `sqlite/storage/branch-entries.ts`.
//!
//! The branch cache is *derived*: `entries.parent_id` stays canonical, and this
//! table only makes root-to-tip scans a single indexed range read instead of a
//! pointer chase. It is never repaired implicitly — a missing or stale cache is
//! an error, and `SqliteSessionRepo::repair_branch_cache` is the explicit fix.

use std::collections::HashSet;

use rusqlite::Connection;
use serde_json::Value;

use pi_session::{EntryOrder, EntryType, SessionError, SessionResult};

use crate::sql::{join_sql_fragments, SqlQuery};

/// Which cached branch a leaf belongs to, and where on it the leaf sits.
#[derive(Debug, Clone)]
pub struct CachedBranch {
    pub branch_id: String,
    pub leaf_seq: i64,
}

/// A cached branch row joined back to its canonical entry.
#[derive(Debug, Clone)]
pub struct CachedBranchEntryRow {
    pub id: String,
    pub entry_seq: i64,
    pub parent_id: Option<String>,
    pub entry_type: String,
    pub timestamp: i64,
    pub payload: String,
}

impl CachedBranchEntryRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            entry_seq: row.get("entry_seq")?,
            parent_id: row.get("parent_id")?,
            entry_type: row.get("type")?,
            timestamp: row.get("timestamp")?,
            payload: row.get("payload")?,
        })
    }
}

/// Filters pushed down into the cached-branch scan.
#[derive(Debug, Clone, Default)]
pub struct CachedBranchQuery {
    pub entry_type: Option<EntryType>,
    pub custom_type: Option<String>,
    pub stop_at_type: Option<EntryType>,
    pub stop_at_id: Option<String>,
    pub cursor: Option<i64>,
    pub order: Option<EntryOrder>,
    pub limit: Option<i64>,
}

pub fn read_cached_branch(
    conn: &Connection,
    session_id: &str,
    leaf_id: &str,
) -> SessionResult<Option<CachedBranch>> {
    let mut query = SqlQuery::raw(
        "SELECT branch_id, entry_seq\n\t\tFROM branch_entries\n\t\tWHERE session_id = ",
    );
    query
        .bind(session_id)
        .push(" AND entry_id = ")
        .bind(leaf_id)
        .push("\n\t\tORDER BY branch_id\n\t\tLIMIT 1");
    query.get(conn, |row| {
        Ok(CachedBranch {
            branch_id: row.get("branch_id")?,
            leaf_seq: row.get("entry_seq")?,
        })
    })
}

/// The bounded, filtered window of a cached branch, newest or oldest first.
///
/// The stop bound is computed inside SQL as an aggregate over the branch so a
/// bounded read never touches — let alone decodes — entries outside the window.
pub fn query_cached_branch_rows(
    conn: &Connection,
    session_id: &str,
    branch: &CachedBranch,
    query: &CachedBranchQuery,
) -> SessionResult<Vec<CachedBranchEntryRow>> {
    let oldest_first = query.order.unwrap_or_default().is_oldest_first();

    let mut stop_predicates: Vec<SqlQuery> = Vec::new();
    if let Some(stop_at_type) = query.stop_at_type {
        let mut fragment = SqlQuery::raw("stop.entry_type = ");
        fragment.bind(stop_at_type.as_str());
        stop_predicates.push(fragment);
    }
    if let Some(stop_at_id) = &query.stop_at_id {
        let mut fragment = SqlQuery::raw("stop.entry_id = ");
        fragment.bind(stop_at_id.as_str());
        stop_predicates.push(fragment);
    }

    let mut predicates: Vec<SqlQuery> = Vec::new();
    predicates.push({
        let mut fragment = SqlQuery::raw("b.session_id = ");
        fragment.bind(session_id);
        fragment
    });
    predicates.push({
        let mut fragment = SqlQuery::raw("b.branch_id = ");
        fragment.bind(branch.branch_id.as_str());
        fragment
    });
    predicates.push({
        let mut fragment = SqlQuery::raw("b.entry_seq <= ");
        fragment.bind(branch.leaf_seq);
        fragment
    });

    if !stop_predicates.is_empty() {
        let mut boundary = SqlQuery::raw(if oldest_first {
            "SELECT MIN(stop.entry_seq)"
        } else {
            "SELECT MAX(stop.entry_seq)"
        });
        boundary.push("\n\t\t\t\tFROM branch_entries AS stop\n\t\t\t\tWHERE stop.session_id = ");
        boundary
            .bind(session_id)
            .push("\n\t\t\t\t\tAND stop.branch_id = ")
            .bind(branch.branch_id.as_str())
            .push("\n\t\t\t\t\tAND stop.entry_seq <= ")
            .bind(branch.leaf_seq)
            .push("\n\t\t\t\t\tAND (");
        boundary.append(&join_sql_fragments(&stop_predicates, " OR "));
        boundary.push(")");

        let mut fragment = SqlQuery::raw("b.entry_seq ");
        fragment.push(if oldest_first { "<=" } else { ">=" });
        fragment.push(" COALESCE((");
        fragment.append(&boundary);
        fragment.push("), ");
        fragment.bind(if oldest_first { branch.leaf_seq } else { 0 });
        fragment.push(")");
        predicates.push(fragment);
    }

    if let Some(cursor) = query.cursor {
        let mut fragment = SqlQuery::raw("b.entry_seq ");
        fragment.push(if oldest_first { "> " } else { "< " });
        fragment.bind(cursor);
        predicates.push(fragment);
    }
    if let Some(entry_type) = query.entry_type {
        let mut fragment = SqlQuery::raw("b.entry_type = ");
        fragment.bind(entry_type.as_str());
        predicates.push(fragment);
    }
    if let Some(custom_type) = &query.custom_type {
        let mut fragment = SqlQuery::raw("b.custom_type = ");
        fragment.bind(custom_type.as_str());
        predicates.push(fragment);
    }

    let mut statement = SqlQuery::raw(
        "SELECT e.session_id, e.id, e.seq AS entry_seq, e.parent_id, e.type, e.timestamp, e.payload
\t\tFROM branch_entries AS b
\t\tJOIN entries AS e ON e.session_id = b.session_id AND e.id = b.entry_id
\t\tWHERE ",
    );
    statement.append(&join_sql_fragments(&predicates, " AND "));
    statement.push("\n\t\tORDER BY b.entry_seq ");
    statement.push(if oldest_first { "ASC" } else { "DESC" });
    if let Some(limit) = query.limit {
        statement.push(" LIMIT ").bind(limit);
    }
    statement.all(conn, CachedBranchEntryRow::from_row)
}

pub fn delete_branch_entries(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM branch_entries WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_branch_entry(
    conn: &Connection,
    session_id: &str,
    branch_id: &str,
    entry_id: &str,
    entry_seq: i64,
    entry_type: &str,
    custom_type: Option<&str>,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "INSERT INTO branch_entries\n\t\t\t\t(session_id, branch_id, entry_id, entry_seq, entry_type, custom_type)\n\t\t\t\tVALUES (",
    );
    query
        .bind(session_id)
        .push(", ")
        .bind(branch_id)
        .push(", ")
        .bind(entry_id)
        .push(", ")
        .bind(entry_seq)
        .push(", ")
        .bind(entry_type)
        .push(", ")
        .bind(custom_type)
        .push(")");
    query.run(conn)?;
    Ok(())
}

struct BranchPathEntryRow {
    id: String,
    seq: i64,
    parent_id: Option<String>,
    entry_type: String,
    payload: String,
}

/// The `custom_type` cache column, decoded from the stored payload.
fn custom_type_from_payload(row: &BranchPathEntryRow) -> SessionResult<Option<String>> {
    if row.entry_type != "custom" {
        return Ok(None);
    }
    let invalid = || {
        SessionError::invalid_entry(format!(
            "Invalid SQLite session entry {}: failed to decode entry {}",
            row.id, row.id
        ))
    };
    let payload: Value = serde_json::from_str(&row.payload).map_err(|_| invalid())?;
    match payload.get("customType") {
        Some(Value::String(custom_type)) => Ok(Some(custom_type.clone())),
        _ => Err(invalid()),
    }
}

/// Walks canonical parent links from `leaf_id` to the root and materializes the
/// whole path as one cached branch.
pub fn insert_branch_entries_for_path(
    conn: &Connection,
    session_id: &str,
    branch_id: &str,
    leaf_id: &str,
) -> SessionResult<()> {
    let mut path: Vec<BranchPathEntryRow> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut entry_id = Some(leaf_id.to_string());

    while let Some(current) = entry_id {
        if !seen.insert(current.clone()) {
            return Err(SessionError::invalid_entry(format!(
                "Entry parent cycle at {current}"
            )));
        }
        let mut query = SqlQuery::raw(
            "SELECT id, seq, parent_id, type, payload\n\t\t\tFROM entries\n\t\t\tWHERE session_id = ",
        );
        query
            .bind(session_id)
            .push(" AND id = ")
            .bind(current.as_str());
        let row = query
            .get(conn, |row| {
                Ok(BranchPathEntryRow {
                    id: row.get("id")?,
                    seq: row.get("seq")?,
                    parent_id: row.get("parent_id")?,
                    entry_type: row.get("type")?,
                    payload: row.get("payload")?,
                })
            })?
            .ok_or_else(|| SessionError::invalid_entry(format!("Entry {current} not found")))?;
        entry_id = row.parent_id.clone();
        path.push(row);
    }

    for row in path.iter().rev() {
        insert_branch_entry(
            conn,
            session_id,
            branch_id,
            &row.id,
            row.seq,
            &row.entry_type,
            custom_type_from_payload(row)?.as_deref(),
        )?;
    }
    Ok(())
}

/// The (branch, position) of any cached branch containing `entry_id`.
pub fn read_branch_containing_entry(
    conn: &Connection,
    session_id: &str,
    entry_id: &str,
) -> SessionResult<Option<CachedBranch>> {
    let mut query = SqlQuery::raw(
        "SELECT b.branch_id, b.entry_seq\n\t\tFROM branch_entries AS b\n\t\tWHERE b.session_id = ",
    );
    query
        .bind(session_id)
        .push(" AND b.entry_id = ")
        .bind(entry_id)
        .push("\n\t\tORDER BY b.branch_id\n\t\tLIMIT 1");
    query.get(conn, |row| {
        Ok(CachedBranch {
            branch_id: row.get("branch_id")?,
            leaf_seq: row.get("entry_seq")?,
        })
    })
}

/// Forks a cached branch: copies the prefix of `source_branch_id` up to and
/// including `through_seq` into `target_branch_id`.
pub fn copy_branch_entries_through_seq(
    conn: &Connection,
    session_id: &str,
    target_branch_id: &str,
    source_branch_id: &str,
    through_seq: i64,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "INSERT INTO branch_entries (session_id, branch_id, entry_id, entry_seq, entry_type, custom_type)\n\t\tSELECT session_id, ",
    );
    query
        .bind(target_branch_id)
        .push(", entry_id, entry_seq, entry_type, custom_type\n\t\tFROM branch_entries\n\t\tWHERE session_id = ")
        .bind(session_id)
        .push(" AND branch_id = ")
        .bind(source_branch_id)
        .push(" AND entry_seq <= ")
        .bind(through_seq);
    query.run(conn)?;
    Ok(())
}
