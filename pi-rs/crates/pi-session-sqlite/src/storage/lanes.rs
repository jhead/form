//! `lanes` and `lane_moves` access. Port of `sqlite/storage/lanes.ts`.

use rusqlite::Connection;

use pi_session::{SessionError, SessionResult};

use crate::sql::SqlQuery;

#[derive(Debug, Clone)]
pub struct LaneRow {
    pub session_id: String,
    pub lane: String,
    pub leaf_id: Option<String>,
    pub open_operation_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LaneMoveRow {
    pub seq: i64,
    pub lane: String,
    pub leaf_id: Option<String>,
}

pub fn create_initial_lane(
    conn: &Connection,
    session_id: &str,
    lane: &str,
    leaf_id: Option<&str>,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw(
        "INSERT INTO lanes (session_id, lane, leaf_id, open_operation_id)\n\t\tVALUES (",
    );
    query
        .bind(session_id)
        .push(", ")
        .bind(lane)
        .push(", ")
        .bind(leaf_id)
        .push(", NULL)");
    query.run(conn)?;
    Ok(())
}

/// Every lane, alphabetically. Fails loudly when a lane points at an entry that
/// no longer exists — the canonical parent links are the source of truth and a
/// dangling leaf means the database was edited underneath us.
pub fn read_lanes(conn: &Connection, session_id: &str) -> SessionResult<Vec<LaneRow>> {
    let mut query = SqlQuery::raw(
        "SELECT
\t\t\tl.session_id,
\t\t\tl.lane,
\t\t\tl.leaf_id,
\t\t\tl.open_operation_id,
\t\t\t(l.leaf_id IS NULL OR EXISTS (
\t\t\t\tSELECT 1 FROM entries AS e WHERE e.session_id = l.session_id AND e.id = l.leaf_id
\t\t\t)) AS leaf_exists
\t\tFROM lanes AS l
\t\tWHERE l.session_id = ",
    );
    query.bind(session_id).push("\n\t\tORDER BY l.lane");
    let rows = query.all(conn, |row| {
        Ok((
            LaneRow {
                session_id: row.get("session_id")?,
                lane: row.get("lane")?,
                leaf_id: row.get("leaf_id")?,
                open_operation_id: row.get("open_operation_id")?,
            },
            row.get::<_, i64>("leaf_exists")?,
        ))
    })?;
    let mut lanes = Vec::with_capacity(rows.len());
    for (lane, leaf_exists) in rows {
        if leaf_exists == 0 {
            return Err(SessionError::storage(format!(
                "Lane {} points at missing entry {}",
                lane.lane,
                lane.leaf_id.as_deref().unwrap_or("")
            )));
        }
        lanes.push(lane);
    }
    Ok(lanes)
}

pub fn read_lane(
    conn: &Connection,
    session_id: &str,
    lane: &str,
) -> SessionResult<Option<LaneRow>> {
    let mut query = SqlQuery::raw(
        "SELECT session_id, lane, leaf_id, open_operation_id\n\t\tFROM lanes\n\t\tWHERE session_id = ",
    );
    query.bind(session_id).push(" AND lane = ").bind(lane);
    query.get(conn, |row| {
        Ok(LaneRow {
            session_id: row.get("session_id")?,
            lane: row.get("lane")?,
            leaf_id: row.get("leaf_id")?,
            open_operation_id: row.get("open_operation_id")?,
        })
    })
}

/// The lane's leaf, validated. `invalid_lane` when the lane is unknown.
pub fn read_lane_head(
    conn: &Connection,
    session_id: &str,
    lane: &str,
) -> SessionResult<Option<String>> {
    let mut query = SqlQuery::raw(
        "SELECT
\t\t\tl.leaf_id,
\t\t\t(l.leaf_id IS NULL OR EXISTS (
\t\t\t\tSELECT 1 FROM entries AS e WHERE e.session_id = l.session_id AND e.id = l.leaf_id
\t\t\t)) AS leaf_exists
\t\tFROM lanes AS l
\t\tWHERE l.session_id = ",
    );
    query.bind(session_id).push(" AND l.lane = ").bind(lane);
    let row = query.get(conn, |row| {
        Ok((
            row.get::<_, Option<String>>("leaf_id")?,
            row.get::<_, i64>("leaf_exists")?,
        ))
    })?;
    let Some((leaf_id, leaf_exists)) = row else {
        return Err(SessionError::invalid_lane(format!(
            "Lane not found: {lane}"
        )));
    };
    if leaf_exists == 0 {
        return Err(SessionError::storage(format!(
            "Entry {} not found",
            leaf_id.as_deref().unwrap_or("")
        )));
    }
    Ok(leaf_id)
}

pub fn create_lane(
    conn: &Connection,
    session_id: &str,
    seq: i64,
    lane: &str,
    leaf_id: Option<&str>,
) -> SessionResult<()> {
    create_initial_lane(conn, session_id, lane, leaf_id)?;
    append_lane_move(conn, session_id, seq, lane, leaf_id)
}

pub fn move_lane(
    conn: &Connection,
    session_id: &str,
    seq: i64,
    lane: &str,
    leaf_id: Option<&str>,
) -> SessionResult<()> {
    set_lane_leaf(conn, session_id, lane, leaf_id)?;
    append_lane_move(conn, session_id, seq, lane, leaf_id)
}

pub fn set_lane_leaf(
    conn: &Connection,
    session_id: &str,
    lane: &str,
    leaf_id: Option<&str>,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw("UPDATE lanes SET leaf_id = ");
    query
        .bind(leaf_id)
        .push(" WHERE session_id = ")
        .bind(session_id)
        .push(" AND lane = ")
        .bind(lane);
    if query.run(conn)? != 1 {
        return Err(SessionError::invalid_lane(format!(
            "Lane not found: {lane}"
        )));
    }
    Ok(())
}

pub fn start_lane_operation(
    conn: &Connection,
    session_id: &str,
    lane: &str,
    run_id: &str,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw("UPDATE lanes SET open_operation_id = ");
    query
        .bind(run_id)
        .push("\n\t\tWHERE session_id = ")
        .bind(session_id)
        .push(" AND lane = ")
        .bind(lane)
        .push(" AND open_operation_id IS NULL");
    if query.run(conn)? == 1 {
        return Ok(());
    }
    let current = read_lane(conn, session_id, lane)?
        .ok_or_else(|| SessionError::invalid_lane(format!("Lane not found: {lane}")))?;
    Err(SessionError::storage(format!(
        "Lane {lane} already has an open operation {}",
        current.open_operation_id.as_deref().unwrap_or("")
    )))
}

pub fn finish_lane_operation(
    conn: &Connection,
    session_id: &str,
    lane: &str,
    run_id: &str,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw("UPDATE lanes SET open_operation_id = NULL");
    query
        .push("\n\t\tWHERE session_id = ")
        .bind(session_id)
        .push(" AND lane = ")
        .bind(lane)
        .push(" AND open_operation_id = ")
        .bind(run_id);
    query.run(conn)?;
    Ok(())
}

pub fn read_lane_move_rows(
    conn: &Connection,
    session_id: &str,
    after_seq: Option<i64>,
    limit: Option<i64>,
) -> SessionResult<Vec<LaneMoveRow>> {
    let mut query = SqlQuery::raw(
        "SELECT session_id, seq, lane, leaf_id\n\t\tFROM lane_moves\n\t\tWHERE session_id = ",
    );
    query.bind(session_id);
    if let Some(after_seq) = after_seq {
        query.push(" AND seq > ").bind(after_seq);
    }
    query.push("\n\t\tORDER BY seq");
    if let Some(limit) = limit {
        query.push(" LIMIT ").bind(limit);
    }
    query.all(conn, |row| {
        Ok(LaneMoveRow {
            seq: row.get("seq")?,
            lane: row.get("lane")?,
            leaf_id: row.get("leaf_id")?,
        })
    })
}

pub fn delete_lane_rows(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut moves = SqlQuery::raw("DELETE FROM lane_moves WHERE session_id = ");
    moves.bind(session_id);
    moves.run(conn)?;
    let mut lanes = SqlQuery::raw("DELETE FROM lanes WHERE session_id = ");
    lanes.bind(session_id);
    lanes.run(conn)?;
    Ok(())
}

fn append_lane_move(
    conn: &Connection,
    session_id: &str,
    seq: i64,
    lane: &str,
    leaf_id: Option<&str>,
) -> SessionResult<()> {
    let mut query =
        SqlQuery::raw("INSERT INTO lane_moves (session_id, seq, lane, leaf_id) VALUES (");
    query
        .bind(session_id)
        .push(", ")
        .bind(seq)
        .push(", ")
        .bind(lane)
        .push(", ")
        .bind(leaf_id)
        .push(")");
    query.run(conn)?;
    Ok(())
}
