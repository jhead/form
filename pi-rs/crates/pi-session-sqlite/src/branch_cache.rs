//! Derived branch cache maintenance. Port of `sqlite/branch-cache.ts`.
//!
//! Appending to a lane either extends the branch whose tip is the parent, or —
//! when the parent is in the middle of an existing branch, i.e. the session just
//! forked — copies that branch's prefix into a fresh branch id. Nothing here
//! repairs a damaged cache implicitly; see
//! [`crate::repo::SqliteSessionRepo::repair_branch_cache`].

use rusqlite::Connection;

use pi_session::{SessionError, SessionResult};

use crate::sql::{savepoint, SqlQuery};
use crate::storage::branch_entries::{
    copy_branch_entries_through_seq, delete_branch_entries, insert_branch_entries_for_path,
    insert_branch_entry, read_branch_containing_entry,
};
use crate::storage::branch_tips::{
    delete_branch_tips, insert_branch_tip, read_branch_tip_branch_id, update_branch_tip,
};

pub fn delete_branch_cache(conn: &Connection, session_id: &str) -> SessionResult<()> {
    delete_branch_tips(conn, session_id)?;
    delete_branch_entries(conn, session_id)
}

/// Rebuilds every branch from the canonical parent links.
pub fn rebuild_branch_cache(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "SELECT leaf.id
\t\tFROM entries AS leaf
\t\tWHERE leaf.session_id = ",
    );
    query.bind(session_id).push(
        "
\t\t\tAND NOT EXISTS (
\t\t\t\tSELECT 1 FROM entries AS child WHERE child.session_id = leaf.session_id AND child.parent_id = leaf.id
\t\t\t)
\t\tORDER BY leaf.seq",
    );
    let tips: Vec<String> = query.all(conn, |row| row.get(0))?;
    delete_branch_cache(conn, session_id)?;
    for tip in tips {
        build_cached_branch(conn, session_id, &tip)?;
    }
    Ok(())
}

/// Materializes the root-to-`leaf_id` path as a new branch.
pub fn build_cached_branch(
    conn: &Connection,
    session_id: &str,
    leaf_id: &str,
) -> SessionResult<()> {
    savepoint(conn, "build_branch_cache", |conn| {
        let branch_id = pi_core::uuidv7();
        insert_branch_entries_for_path(conn, session_id, &branch_id, leaf_id)?;
        insert_branch_tip(conn, session_id, leaf_id, &branch_id)
    })
    .map_err(|error| match error {
        // `SessionError`s already carry the right code; only driver failures get
        // rewritten, matching upstream's `instanceof SessionError` check.
        error if error.code() != "storage" => error,
        error => SessionError::storage(format!(
            "Failed to build SQLite branch cache at entry {leaf_id}: {error}"
        )),
    })
}

#[allow(clippy::too_many_arguments)]
fn extend_branch(
    conn: &Connection,
    session_id: &str,
    branch_id: &str,
    parent_id: &str,
    entry_id: &str,
    entry_seq: i64,
    entry_type: &str,
    custom_type: Option<&str>,
) -> SessionResult<()> {
    insert_branch_entry(
        conn,
        session_id,
        branch_id,
        entry_id,
        entry_seq,
        entry_type,
        custom_type,
    )?;
    if !update_branch_tip(conn, session_id, branch_id, parent_id, entry_id)? {
        return Err(SessionError::invalid_entry(format!(
            "Branch tip {parent_id} changed during append"
        )));
    }
    Ok(())
}

/// Records a freshly appended entry in the cache.
#[allow(clippy::too_many_arguments)]
pub fn append_entry_to_branch_cache(
    conn: &Connection,
    session_id: &str,
    entry_id: &str,
    entry_seq: i64,
    entry_type: &str,
    custom_type: Option<&str>,
    parent_id: Option<&str>,
) -> SessionResult<()> {
    let Some(parent_id) = parent_id else {
        // A root entry starts its own branch.
        let branch_id = pi_core::uuidv7();
        insert_branch_entry(
            conn,
            session_id,
            &branch_id,
            entry_id,
            entry_seq,
            entry_type,
            custom_type,
        )?;
        return insert_branch_tip(conn, session_id, entry_id, &branch_id);
    };

    if let Some(tip_branch_id) = read_branch_tip_branch_id(conn, session_id, parent_id)? {
        return extend_branch(
            conn,
            session_id,
            &tip_branch_id,
            parent_id,
            entry_id,
            entry_seq,
            entry_type,
            custom_type,
        );
    }

    // The parent is mid-branch: this append forks, so copy the prefix.
    let source = read_branch_containing_entry(conn, session_id, parent_id)?.ok_or_else(|| {
        SessionError::invalid_entry(format!(
            "Branch cache has no branch containing parent entry {parent_id}"
        ))
    })?;
    let branch_id = pi_core::uuidv7();
    copy_branch_entries_through_seq(
        conn,
        session_id,
        &branch_id,
        &source.branch_id,
        source.leaf_seq,
    )?;
    insert_branch_entry(
        conn,
        session_id,
        &branch_id,
        entry_id,
        entry_seq,
        entry_type,
        custom_type,
    )?;
    insert_branch_tip(conn, session_id, entry_id, &branch_id)
}
