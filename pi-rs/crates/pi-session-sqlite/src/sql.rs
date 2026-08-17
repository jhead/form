//! Composable parameterized queries. Port of `sqlite/sql.ts`.
//!
//! Upstream builds SQL with a tagged template that turns every interpolation
//! into a `?` parameter and inlines nested queries with their parameters in
//! order. Rust has no tagged templates, so the same shape is a small builder:
//! [`SqlQuery::push`] appends trusted text, [`SqlQuery::bind`] appends a `?`
//! plus its parameter, and [`SqlQuery::append`] splices another fragment
//! without renumbering anything (SQLite binds positionally, so concatenation is
//! all that is needed).

use rusqlite::types::{ToSqlOutput, Value};
use rusqlite::{Connection, Row, ToSql};

use pi_session::SessionError;
use pi_session::SessionResult;

/// A parameterized SQLite query. Port of upstream's `SqlQuery`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SqlQuery {
    text: String,
    params: Vec<Value>,
}

impl SqlQuery {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fragment of trusted SQL text with no parameters.
    pub fn raw(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            params: Vec::new(),
        }
    }

    /// A single `?` bound to `value`.
    pub fn param(value: impl ToSql) -> Self {
        let mut query = Self::new();
        query.bind(value);
        query
    }

    pub fn query_text(&self) -> &str {
        &self.text
    }

    pub fn params(&self) -> &[Value] {
        &self.params
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.params.is_empty()
    }

    /// Append trusted SQL text.
    pub fn push(&mut self, text: &str) -> &mut Self {
        self.text.push_str(text);
        self
    }

    /// Append `?` and its parameter.
    ///
    /// Parameters are stored owned so a fragment can outlive the value it was
    /// built from; `ToSql` is the widest input type that covers `&str`,
    /// `String`, the integer types and `Option<_>` uniformly.
    pub fn bind(&mut self, value: impl ToSql) -> &mut Self {
        self.text.push('?');
        self.params.push(owned_value(value));
        self
    }

    /// Splice another fragment in, preserving parameter order.
    pub fn append(&mut self, other: &SqlQuery) -> &mut Self {
        self.text.push_str(&other.text);
        self.params.extend(other.params.iter().cloned());
        self
    }

    fn sql_params(&self) -> Vec<ToSqlOutput<'_>> {
        self.params
            .iter()
            .map(|value| ToSqlOutput::Borrowed(value.into()))
            .collect()
    }

    /// `db.exec` — no parameters allowed, matching upstream.
    pub fn exec(&self, conn: &Connection) -> SessionResult<()> {
        debug_assert!(
            self.params.is_empty(),
            "SQLite exec queries cannot have parameters"
        );
        conn.execute_batch(&self.text)
            .map_err(|error| sqlite_error(&self.text, error))
    }

    /// Runs a statement, returning the number of changed rows.
    pub fn run(&self, conn: &Connection) -> SessionResult<usize> {
        let params = self.sql_params();
        conn.execute(&self.text, rusqlite::params_from_iter(params))
            .map_err(|error| sqlite_error(&self.text, error))
    }

    /// First row, or `None`.
    pub fn get<T, F>(&self, conn: &Connection, decode: F) -> SessionResult<Option<T>>
    where
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        let mut statement = conn
            .prepare(&self.text)
            .map_err(|error| sqlite_error(&self.text, error))?;
        let params = self.sql_params();
        let mut rows = statement
            .query(rusqlite::params_from_iter(params))
            .map_err(|error| sqlite_error(&self.text, error))?;
        match rows.next().map_err(|e| sqlite_error(&self.text, e))? {
            Some(row) => Ok(Some(
                decode(row).map_err(|error| sqlite_error(&self.text, error))?,
            )),
            None => Ok(None),
        }
    }

    /// Every row.
    pub fn all<T, F>(&self, conn: &Connection, mut decode: F) -> SessionResult<Vec<T>>
    where
        F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
    {
        let mut statement = conn
            .prepare(&self.text)
            .map_err(|error| sqlite_error(&self.text, error))?;
        let params = self.sql_params();
        let mut rows = statement
            .query(rusqlite::params_from_iter(params))
            .map_err(|error| sqlite_error(&self.text, error))?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().map_err(|e| sqlite_error(&self.text, e))? {
            results.push(decode(row).map_err(|error| sqlite_error(&self.text, error))?);
        }
        Ok(results)
    }

    /// `SELECT 1 ... LIMIT 1` style existence probe.
    pub fn exists(&self, conn: &Connection) -> SessionResult<bool> {
        Ok(self.get(conn, |_| Ok(()))?.is_some())
    }
}

/// Every value this crate binds is a scalar whose `to_sql` is infallible, so
/// the error arm is unreachable in practice; `Null` keeps the builder total
/// rather than making every call site fallible.
fn owned_value(value: impl ToSql) -> Value {
    match value.to_sql() {
        Ok(ToSqlOutput::Borrowed(borrowed)) => Value::from(borrowed),
        Ok(ToSqlOutput::Owned(owned)) => owned,
        _ => {
            debug_assert!(false, "SQLite parameter is not a scalar value");
            Value::Null
        }
    }
}

/// Joins trusted query fragments while preserving their parameter order.
/// Port of `joinSqlFragments`.
pub fn join_sql_fragments(fragments: &[SqlQuery], separator: &str) -> SqlQuery {
    let mut joined = SqlQuery::new();
    for (index, fragment) in fragments.iter().enumerate() {
        if index > 0 {
            joined.push(separator);
        }
        joined.append(fragment);
    }
    joined
}

/// Every `rusqlite` failure that is not otherwise classified surfaces as
/// `storage`, carrying SQLite's own message — upstream rethrows the driver
/// error verbatim and its tests assert on those strings (`branch insert
/// failed`, `fail fork`, ...).
pub fn sqlite_error(statement: &str, error: rusqlite::Error) -> SessionError {
    let first_line = statement.trim_start().lines().next().unwrap_or("").trim();
    SessionError::storage(format!("{error} (while running: {first_line})"))
}

/// `BEGIN IMMEDIATE` / `COMMIT` / `ROLLBACK`, spelled out rather than using
/// `rusqlite::Transaction` so nested savepoints and the storage helpers can all
/// share a plain `&Connection`.
pub fn transaction<T, F>(conn: &Connection, body: F) -> SessionResult<T>
where
    F: FnOnce(&Connection) -> SessionResult<T>,
{
    SqlQuery::raw("BEGIN IMMEDIATE").exec(conn)?;
    match body(conn) {
        Ok(value) => {
            SqlQuery::raw("COMMIT").exec(conn)?;
            Ok(value)
        }
        Err(error) => {
            // Ignore rollback errors so the original failure survives.
            let _ = SqlQuery::raw("ROLLBACK").exec(conn);
            Err(error)
        }
    }
}

/// `SAVEPOINT name` / `RELEASE` / `ROLLBACK TO`, for work nested inside an
/// open transaction.
pub fn savepoint<T, F>(conn: &Connection, name: &str, body: F) -> SessionResult<T>
where
    F: FnOnce(&Connection) -> SessionResult<T>,
{
    SqlQuery::raw(format!("SAVEPOINT {name}")).exec(conn)?;
    match body(conn) {
        Ok(value) => {
            SqlQuery::raw(format!("RELEASE SAVEPOINT {name}")).exec(conn)?;
            Ok(value)
        }
        Err(error) => {
            if SqlQuery::raw(format!("ROLLBACK TO SAVEPOINT {name}"))
                .exec(conn)
                .is_ok()
            {
                let _ = SqlQuery::raw(format!("RELEASE SAVEPOINT {name}")).exec(conn);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of `sql.test.ts`.
    #[test]
    fn composes_queries_without_renumbering_parameters() {
        let conn = Connection::open_in_memory().unwrap();
        SqlQuery::raw("CREATE TABLE entries (id TEXT PRIMARY KEY, kind TEXT NOT NULL, active INTEGER NOT NULL)")
            .exec(&conn)
            .unwrap();
        let mut insert = SqlQuery::raw("INSERT INTO entries (id, kind, active) VALUES (");
        insert.bind("one").push(", ").bind("message");
        insert.push(", ").bind(1i64).push(")");
        insert.run(&conn).unwrap();
        let mut insert = SqlQuery::raw("INSERT INTO entries (id, kind, active) VALUES (");
        insert.bind("two").push(", ").bind("message");
        insert.push(", ").bind(0i64).push(")");
        insert.run(&conn).unwrap();

        let mut kind = SqlQuery::raw("kind = ");
        kind.bind("message");
        let mut active = SqlQuery::raw("active = ");
        active.bind(1i64);
        let filters = join_sql_fragments(&[kind, active], " AND ");

        let mut query = SqlQuery::raw("SELECT id FROM entries WHERE ");
        query.append(&filters).push(" LIMIT ").bind(10i64);
        let ids: Vec<String> = query.all(&conn, |row| row.get(0)).unwrap();
        assert_eq!(ids, vec!["one".to_string()]);
    }

    #[test]
    fn executes_parameterized_queries() {
        let conn = Connection::open_in_memory().unwrap();
        SqlQuery::raw("CREATE TABLE values_table (id INTEGER PRIMARY KEY, value TEXT NOT NULL)")
            .exec(&conn)
            .unwrap();
        for (id, value) in [(1i64, "one"), (2, "two")] {
            let mut insert = SqlQuery::raw("INSERT INTO values_table (id, value) VALUES (");
            insert.bind(id).push(", ").bind(value).push(")");
            insert.run(&conn).unwrap();
        }

        let mut single = SqlQuery::raw("SELECT value FROM values_table WHERE id = ");
        single.bind(1i64);
        assert_eq!(
            single.get(&conn, |row| row.get::<_, String>(0)).unwrap(),
            Some("one".to_string())
        );
        let all: Vec<String> = SqlQuery::raw("SELECT value FROM values_table ORDER BY id")
            .all(&conn, |row| row.get(0))
            .unwrap();
        assert_eq!(all, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn commits_and_rolls_back_transactions() {
        let conn = Connection::open_in_memory().unwrap();
        SqlQuery::raw("CREATE TABLE values_table (value INTEGER NOT NULL)")
            .exec(&conn)
            .unwrap();
        let committed = transaction(&conn, |conn| {
            let mut insert = SqlQuery::raw("INSERT INTO values_table (value) VALUES (");
            insert.bind(42i64).push(")");
            insert.run(conn)?;
            Ok("committed")
        })
        .unwrap();
        assert_eq!(committed, "committed");

        let failed: SessionResult<()> = transaction(&conn, |conn| {
            let mut insert = SqlQuery::raw("INSERT INTO values_table (value) VALUES (");
            insert.bind(7i64).push(")");
            insert.run(conn)?;
            Err(SessionError::storage("rolled back"))
        });
        assert!(failed.is_err());

        let values: Vec<i64> = SqlQuery::raw("SELECT value FROM values_table")
            .all(&conn, |row| row.get(0))
            .unwrap();
        assert_eq!(values, vec![42]);
    }
}
