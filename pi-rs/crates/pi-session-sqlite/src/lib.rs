//! SQLite session backend for the Pi SDK.
//!
//! Port of `.upstream/packages/session-backends/sqlite-node/src/sqlite/`,
//! implementing the `pi-session` storage contract
//! ([`pi_session::repo::SessionRepo`], [`pi_session::repo::SessionStorage`],
//! [`pi_session::repo::SearchBackend`]).
//!
//! ## Layout
//!
//! | module | upstream |
//! |---|---|
//! | [`sql`] | `sqlite/sql.ts` |
//! | [`migrations`] | `sqlite/migrations.ts` + `migrations/001_initial.sql` |
//! | [`storage`] | `sqlite/storage/*.ts` |
//! | [`branch_cache`] | `sqlite/branch-cache.ts` |
//! | [`repo`] | `sqlite/repo.ts` |
//! | [`search_backend`] | `sqlite/search-backend.ts` |
//!
//! ## Compatibility
//!
//! The schema, the table and column names, the migration ids and the JSON
//! encoding of every `payload` column match the TypeScript backend exactly. A
//! database written by either implementation opens in the other.
//!
//! ## Concurrency
//!
//! `rusqlite` is synchronous, so the async trait methods hand their work to
//! [`tokio::task::spawn_blocking`] while holding a fair per-repository
//! connection mutex. Cross-process safety comes from the fenced
//! `writer_leases` table, ported verbatim. See [`repo`] for the details.
//!
//! ```no_run
//! use std::sync::Arc;
//! use pi_session::{SessionCreateOptions, repo::SessionRepo};
//! use pi_session_sqlite::SqliteSessionRepo;
//!
//! # async fn example() -> pi_session::SessionResult<()> {
//! let repo = SqliteSessionRepo::new("/tmp/pi/sessions.sqlite");
//! let session = repo
//!     .create(&SessionCreateOptions::new().with_cwd("/tmp/pi"))
//!     .await?;
//! println!("{}", session.get_metadata().await?.id);
//! # Ok(())
//! # }
//! ```

pub mod branch_cache;
pub mod migrations;
pub mod repo;
pub mod search_backend;
pub mod sql;
pub mod storage;

pub use migrations::{apply_migrations, load_migrations, SqliteMigration};
pub use repo::{SqliteSessionRepo, SqliteSessionStorage, SqliteWriterLeaseOptions};
pub use search_backend::SqliteSearchBackend;
pub use sql::{join_sql_fragments, SqlQuery};
