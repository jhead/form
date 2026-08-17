//! `branch_tips` access. Port of `sqlite/storage/branch-tips.ts`.

use rusqlite::Connection;

use pi_session::SessionResult;

use crate::sql::SqlQuery;

pub fn read_branch_tip_ids(conn: &Connection, session_id: &str) -> SessionResult<Vec<String>> {
    let mut query = SqlQuery::raw("SELECT tip_id FROM branch_tips WHERE session_id = ");
    query.bind(session_id).push(" ORDER BY tip_id");
    query.all(conn, |row| row.get(0))
}

pub fn read_branch_tip_branch_id(
    conn: &Connection,
    session_id: &str,
    tip_id: &str,
) -> SessionResult<Option<String>> {
    let mut query = SqlQuery::raw("SELECT branch_id FROM branch_tips WHERE session_id = ");
    query.bind(session_id).push(" AND tip_id = ").bind(tip_id);
    query.get(conn, |row| row.get(0))
}

pub fn insert_branch_tip(
    conn: &Connection,
    session_id: &str,
    tip_id: &str,
    branch_id: &str,
) -> SessionResult<()> {
    let mut query =
        SqlQuery::raw("INSERT INTO branch_tips (session_id, tip_id, branch_id) VALUES (");
    query
        .bind(session_id)
        .push(", ")
        .bind(tip_id)
        .push(", ")
        .bind(branch_id)
        .push(")");
    query.run(conn)?;
    Ok(())
}

/// Moves a branch's tip. Returns `false` when the compare-and-set missed, which
/// means another writer advanced the branch concurrently.
pub fn update_branch_tip(
    conn: &Connection,
    session_id: &str,
    branch_id: &str,
    old_tip_id: &str,
    new_tip_id: &str,
) -> SessionResult<bool> {
    let mut query = SqlQuery::raw("UPDATE branch_tips SET tip_id = ");
    query
        .bind(new_tip_id)
        .push("\n\t\tWHERE session_id = ")
        .bind(session_id)
        .push(" AND branch_id = ")
        .bind(branch_id)
        .push(" AND tip_id = ")
        .bind(old_tip_id);
    Ok(query.run(conn)? == 1)
}

pub fn delete_branch_tips(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM branch_tips WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}
