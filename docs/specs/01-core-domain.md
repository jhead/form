# Spec 01 — Core domain and store (`form-core::app`)

> **Workstream W1.** Owns `core/crates/form-core/src/app/`, `.../store/`, `.../search/`,
> `.../workspace/` and their tests. Does **not** touch `protocol.rs`, `harness/`, `stats/`,
> `catalog/`, `markdown/`, or anything under `form-ffi`.

## 1. Responsibility

Everything about *sessions as data*: creation, persistence, ordering, grouping, branching,
search, workspace roots, attachment records. The store is the single source of truth; the
harness (W2) appends to it, the stats engine (W3) reads from it, the FFI (W6) exposes it.

## 2. Storage

**SQLite via `rusqlite` (`bundled` feature).** Chosen over JSONL because the Home dashboard
(F11) needs grouped aggregates over months of history and `⌘K` needs full-text search — both
are one query in SQLite and a full scan otherwise.

Database at `{dataDir}/form.sqlite`, WAL mode, `foreign_keys = ON`, `busy_timeout = 5000`.

### Schema (v1)

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);  -- schema_version, …

CREATE TABLE groups (
  id TEXT PRIMARY KEY, name TEXT NOT NULL,
  idx INTEGER NOT NULL, collapsed INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  title_is_custom INTEGER NOT NULL DEFAULT 0,
  group_id TEXT REFERENCES groups(id) ON DELETE SET NULL,
  idx INTEGER NOT NULL,
  workspace_root TEXT,
  provider_id TEXT NOT NULL, model_id TEXT NOT NULL, thinking_level TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'idle',
  archived INTEGER NOT NULL DEFAULT 0,
  pinned INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX sessions_group_idx ON sessions(group_id, idx);
CREATE INDEX sessions_updated ON sessions(updated_at DESC);

-- Append-only transcript log. `payload` is the serialized Entry (spec 00 §6).
CREATE TABLE entries (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  parent_id TEXT,
  kind TEXT NOT NULL,          -- message | model_change | thinking_level_change | compaction | branch_summary | custom
  role TEXT,                   -- user | assistant | toolResult, when kind='message'
  timestamp INTEGER NOT NULL,
  payload TEXT NOT NULL
);
CREATE UNIQUE INDEX entries_seq ON entries(session_id, seq);

-- Per-turn metrics; the stats engine reads this, never the payload blobs.
CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL,
  provider_id TEXT NOT NULL, model_id TEXT NOT NULL, thinking_level TEXT NOT NULL,
  started_at INTEGER NOT NULL, ended_at INTEGER NOT NULL,
  ttft_ms INTEGER, duration_ms INTEGER NOT NULL,
  input INTEGER NOT NULL, output INTEGER NOT NULL,
  cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
  reasoning INTEGER, total_tokens INTEGER NOT NULL,
  cost_total REAL NOT NULL,
  outcome TEXT NOT NULL          -- completed | aborted | failed
);
CREATE INDEX turns_started ON turns(started_at);

CREATE TABLE tool_invocations (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, turn_id TEXT NOT NULL,
  tool_name TEXT NOT NULL, started_at INTEGER NOT NULL,
  duration_ms INTEGER NOT NULL, is_error INTEGER NOT NULL
);

CREATE TABLE attachments (
  id TEXT PRIMARY KEY, session_id TEXT, sha256 TEXT NOT NULL,
  filename TEXT NOT NULL, mime TEXT NOT NULL, bytes INTEGER NOT NULL,
  width INTEGER, height INTEGER,
  path TEXT NOT NULL,            -- {dataDir}/attachments/{sha256}
  thumb_path TEXT,               -- written by Swift, recorded here
  created_at INTEGER NOT NULL
);
CREATE INDEX attachments_sha ON attachments(sha256);

CREATE TABLE recent_roots (path TEXT PRIMARY KEY, last_used INTEGER NOT NULL);

CREATE VIRTUAL TABLE search USING fts5(
  title, body, session_id UNINDEXED, entry_id UNINDEXED, tokenize='porter unicode61'
);
```

Migrations are a `Vec<fn(&Connection)>` indexed by `schema_version`; v1 is the initial
create. Adding a migration must never rewrite an existing one.

## 3. Public API

```rust
pub struct Store { /* Arc<Mutex<Connection>> or a small pool */ }

impl Store {
    pub fn open(data_dir: &Path) -> Result<Self, CoreError>;
    pub fn list_sessions(&self, include_archived: bool) -> Result<SessionList, CoreError>;
    pub fn get_session(&self, id: &str) -> Result<Session, CoreError>;
    pub fn create_session(&self, req: CreateSession) -> Result<SessionSummary, CoreError>;
    pub fn append_entry(&self, session_id: &str, entry: ProvisionedEntry) -> Result<Entry, CoreError>;
    pub fn record_turn(&self, turn: TurnRecord) -> Result<(), CoreError>;
    pub fn move_session(&self, id: &str, group: Option<&str>, index: u32) -> Result<(), CoreError>;
    pub fn search(&self, q: &str, scope: SearchScope, limit: usize) -> Result<Vec<SearchHit>, CoreError>;
    // rename/delete/archive/pin, group CRUD, attachment CRUD, recent roots…
}
```

Rules from `pi-rs/AGENTS.md` apply verbatim: no lifetimes or generics in public signatures,
owned `'static + Send + Sync` types, flat error enums with `code() -> &'static str`, never
leak `anyhow`.

## 4. Behaviours

- **Ordering.** `idx` is a dense integer per group. `move_session` renumbers within a
  transaction. Ungrouped sessions use `group_id IS NULL` and share one sequence.
- **Titles (F2.6).** On the first user message, derive a title: first line, trimmed,
  collapsed whitespace, ≤ 60 chars, sentence-cased, trailing punctuation stripped. Skip if
  `title_is_custom`. `rename_session` sets `title_is_custom = 1`.
- **Search (F13.3).** `search` mirrors titles and message text into FTS5 on append. Hits
  return `{sessionId, entryId?, title, snippet, score}` with `snippet()` markers converted
  to explicit `{start,len}` ranges — Swift must not parse markup out of a string.
- **Branching (F1.5).** `branch_from_message` copies entries up to `entryId` into a new
  session and writes a `branch_summary` entry. Parent linkage is `parent_id`.
- **Status.** `sessions.status` is written by the harness through the store, so a crash mid-
  run leaves a recoverable `error` state rather than a stuck `streaming` one — clear stale
  `streaming` rows to `idle` on open.
- **Attachments (F3).** `add_attachment` hashes content, writes
  `{dataDir}/attachments/{sha256}` if absent (dedupe), and inserts a row. Reject > 10 MB or
  a mime outside the allowlist with `CoreError::AttachmentRejected { reason }` (F3.6).

## 5. Workspace confinement (F4.3)

```rust
pub fn resolve_in_workspace(root: Option<&Path>, candidate: &str)
    -> Result<ResolvedPath, CoreError>;
```

Canonicalize root; join candidate; canonicalize the result (resolving symlinks); require the
canonical result to be `root` or a descendant. Reject: `..` escapes, absolute paths outside
root, symlinks pointing out, and — on macOS — case-insensitive collisions that would slip a
prefix check. `root == None` yields `insideRoot: false` and is allowed but flagged (F4.5).

Tests must cover each rejection class explicitly; this is the API real tools will call.

## 6. Seeding

`seedMockData: true` populates a believable corpus **only when the database is empty**:
3 groups, ~24 sessions with realistic titles, 2–30 turns each, spread over 120 days with a
plausible diurnal and weekly rhythm, several models, some aborted/failed runs, a handful of
tool invocations per turn, and a few image attachments. Seeding is deterministic given a
fixed RNG seed so screenshots and tests are stable.

This corpus is what makes the Home dashboard (F11) meaningful on first launch — treat it as
a product surface, not test scaffolding.

## 7. Done when

- `cargo test -p form-core` covers: migration from empty, CRUD round trips, ordering after
  moves, title derivation, FTS ranking and snippet ranges, branch copying, attachment
  dedupe, every confinement rejection class, and deterministic seeding.
- `cargo clippy -p form-core -- -D warnings` is clean.
