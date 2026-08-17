//! Shared fixtures for the SQLite-specific tests. Port of `test/test-utils.ts`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pi_core::{
    AssistantContent, AssistantMessage, Cost, InputContent, StopReason, Usage, UserContent,
    UserMessage,
};
use pi_session::{
    AgentMessage, BranchSummaryEntry, CompactionEntry, Entry, EntryPayload, EntryQuery,
    ProvisionedEntry, Session, SessionCreateOptions, SessionResult,
};
use pi_session_sqlite::{SqliteSessionRepo, SqliteWriterLeaseOptions};
use rusqlite::Connection;
use tempfile::TempDir;

/// A temp root plus a repository over `<root>/sessions.sqlite`.
pub struct Fixture {
    pub root: TempDir,
    pub repo: SqliteSessionRepo,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_lease(SqliteWriterLeaseOptions::default())
    }

    pub fn with_lease(lease: SqliteWriterLeaseOptions) -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        let repo = SqliteSessionRepo::with_writer_lease(root.path().join("sessions.sqlite"), lease)
            .expect("valid lease options");
        Self { root, repo }
    }

    pub fn database_path(&self) -> PathBuf {
        self.root.path().join("sessions.sqlite")
    }

    pub fn cwd(&self) -> String {
        self.root.path().display().to_string()
    }

    pub fn create_options(&self, id: &str) -> SessionCreateOptions {
        SessionCreateOptions::new().with_id(id).with_cwd(self.cwd())
    }

    pub async fn create(&self, id: &str) -> Session {
        self.repo
            .create(&self.create_options(id))
            .await
            .expect("create session")
    }

    /// A second repository over the same database file — the stand-in for
    /// upstream's "another process".
    pub fn peer_repo(&self, lease: SqliteWriterLeaseOptions) -> SqliteSessionRepo {
        SqliteSessionRepo::with_writer_lease(self.database_path(), lease)
            .expect("valid lease options")
    }

    /// Direct SQL access, mirroring the upstream tests that poke the database
    /// behind the repository's back.
    pub fn inspect<T>(&self, body: impl FnOnce(&Connection) -> T) -> T {
        inspect_at(&self.database_path(), body)
    }
}

pub fn inspect_at<T>(path: &Path, body: impl FnOnce(&Connection) -> T) -> T {
    let conn = Connection::open(path).expect("open database");
    let result = body(&conn);
    conn.close().expect("close database");
    result
}

pub use pi_session::repo::SessionRepo;

pub fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        content: UserContent::Blocks(vec![InputContent::text(text)]),
        timestamp: 1,
    })
}

pub fn assistant_message(text: &str) -> AgentMessage {
    assistant_message_with_usage(text, Usage::default())
}

pub fn assistant_message_with_usage(text: &str, usage: Usage) -> AgentMessage {
    AgentMessage::Assistant(AssistantMessage {
        content: vec![AssistantContent::text(text)],
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: "claude-sonnet-4-5".into(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage,
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
    })
}

pub fn usage(input: i64, output: i64, cache_read: i64, cache_write: i64, cost: Cost) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: input + output + cache_read + cache_write,
        cost,
    }
}

pub fn cost(input: f64, output: f64, cache_read: f64, cache_write: f64, total: f64) -> Cost {
    Cost {
        input,
        output,
        cache_read,
        cache_write,
        total,
    }
}

/// `appendSqliteCompaction`.
pub async fn append_compaction(
    session: &Session,
    summary: &str,
    tokens_before: i64,
    usage: Option<Usage>,
) -> SessionResult<String> {
    let provisioned = ProvisionedEntry::new(
        session.id_generator().next(),
        EntryPayload::Compaction(CompactionEntry {
            summary: summary.into(),
            retained_tail: vec![],
            tokens_before,
            details: None,
            usage,
        }),
    );
    Ok(session.append_entry(&provisioned, "main").await?.id)
}

/// `moveSqliteMainLane`.
pub async fn move_main_lane(
    session: &Session,
    entry_id: Option<&str>,
    summary: Option<(&str, Option<Usage>)>,
) -> SessionResult<Option<String>> {
    session.move_lane("main", entry_id).await?;
    let Some((summary, usage)) = summary else {
        return Ok(None);
    };
    let provisioned = ProvisionedEntry::new(
        session.id_generator().next(),
        EntryPayload::BranchSummary(BranchSummaryEntry {
            from_id: entry_id.unwrap_or("root").into(),
            summary: summary.into(),
            details: None,
            usage,
        }),
    );
    Ok(Some(session.append_entry(&provisioned, "main").await?.id))
}

/// `getSqliteBranch` — the compacted window of the current branch, oldest first.
pub async fn branch_window(session: &Session) -> SessionResult<Vec<Entry>> {
    let Some(start) = session.get_leaf_id().await? else {
        return Ok(Vec::new());
    };
    let mut entries = session
        .find_entries_on_branch(
            &pi_session::BranchQuery::new()
                .with_start(start)
                .with_stop_at_type(pi_session::EntryType::Compaction),
        )
        .await?;
    entries.reverse();
    Ok(entries)
}

/// `getSqliteEntries`.
pub async fn all_entries(session: &Session) -> SessionResult<Vec<Entry>> {
    session
        .find_entries(&EntryQuery::new().with_order(pi_session::EntryOrder::OldestFirst))
        .await
}

pub fn ids(entries: &[Entry]) -> Vec<String> {
    entries.iter().map(|entry| entry.id.clone()).collect()
}
