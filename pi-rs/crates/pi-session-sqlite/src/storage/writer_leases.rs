//! `writer_leases` access. Port of `sqlite/storage/writer-leases.ts`.
//!
//! One claim per session, taken when the session is opened and renewed on every
//! write. `fence` is what makes a takeover safe: acquiring bumps it, so a stale
//! owner whose lease expired can never write again — its renew predicate no
//! longer matches and the write fails instead of racing the new owner.

use rusqlite::Connection;

use pi_session::SessionResult;

use crate::sql::SqlQuery;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterLease {
    pub owner_id: String,
    pub fence: i64,
    pub expires_at_ms: i64,
}

/// Takes the claim, or returns `None` when a live lease is held by someone
/// else. The `ON CONFLICT ... WHERE expires_at_ms <= now` guard makes this a
/// single atomic statement.
pub fn acquire_writer_lease(
    conn: &Connection,
    session_id: &str,
    owner_id: &str,
    now: i64,
    expires_at_ms: i64,
) -> SessionResult<Option<WriterLease>> {
    let mut query = SqlQuery::raw(
        "INSERT INTO writer_leases (session_id, owner_id, fence, expires_at_ms)\n\t\tVALUES (",
    );
    query
        .bind(session_id)
        .push(", ")
        .bind(owner_id)
        .push(", 1, ")
        .bind(expires_at_ms)
        .push(
            ")\n\t\tON CONFLICT(session_id) DO UPDATE SET\n\t\t\towner_id = excluded.owner_id,\n\t\t\tfence = writer_leases.fence + 1,\n\t\t\texpires_at_ms = excluded.expires_at_ms\n\t\tWHERE writer_leases.expires_at_ms <= ",
        )
        .bind(now)
        .push("\n\t\tRETURNING owner_id, fence, expires_at_ms");
    query.get(conn, |row| {
        Ok(WriterLease {
            owner_id: row.get("owner_id")?,
            fence: row.get("fence")?,
            expires_at_ms: row.get("expires_at_ms")?,
        })
    })
}

/// Extends a lease we still own. `false` means the lease was lost.
pub fn renew_writer_lease(
    conn: &Connection,
    session_id: &str,
    lease: &mut WriterLease,
    now: i64,
    expires_at_ms: i64,
) -> SessionResult<bool> {
    let mut query = SqlQuery::raw("UPDATE writer_leases\n\t\tSET expires_at_ms = ");
    query
        .bind(expires_at_ms)
        .push("\n\t\tWHERE session_id = ")
        .bind(session_id)
        .push("\n\t\t\tAND owner_id = ")
        .bind(lease.owner_id.as_str())
        .push("\n\t\t\tAND fence = ")
        .bind(lease.fence)
        .push("\n\t\t\tAND expires_at_ms > ")
        .bind(now);
    let renewed = query.run(conn)? == 1;
    if renewed {
        lease.expires_at_ms = expires_at_ms;
    }
    Ok(renewed)
}

/// Releases our own claim. Deliberately fence-scoped: a stale owner closing
/// must not delete the new owner's lease.
pub fn release_writer_lease(
    conn: &Connection,
    session_id: &str,
    lease: &WriterLease,
) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM writer_leases\n\t\tWHERE session_id = ");
    query
        .bind(session_id)
        .push(" AND owner_id = ")
        .bind(lease.owner_id.as_str())
        .push(" AND fence = ")
        .bind(lease.fence);
    query.run(conn)?;
    Ok(())
}

pub fn delete_writer_lease(conn: &Connection, session_id: &str) -> SessionResult<()> {
    let mut query = SqlQuery::raw("DELETE FROM writer_leases WHERE session_id = ");
    query.bind(session_id);
    query.run(conn)?;
    Ok(())
}
