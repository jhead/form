//! Session state, the append-only JSONL store, branch/lane bookkeeping and
//! context compaction.
//!
//! Port of `.upstream/packages/agent/src/harness/session/` and
//! `harness/compaction/`.
//!
//! ## Layout
//!
//! | module | upstream |
//! |---|---|
//! | [`types`] | `session/types.ts` |
//! | [`state`] | `session/state.ts` |
//! | [`repo`] | the `SessionStorage` / `SessionRepo` / `SessionSearch` contracts |
//! | [`session`] | `session/session.ts` |
//! | [`memory`] | `session/memory.ts` |
//! | [`jsonl`] | `session/jsonl/*` |
//! | [`context`] | `session/context.ts` |
//! | [`reducer`] | `harness/reducer.ts` |
//! | [`messages`] | `harness/messages.ts` (the `AgentMessage` union) |
//! | [`compaction`] | `harness/compaction/*` |
//! | [`search`] | `search/scanning.ts` |
//! | [`testing`] | `session/testing/conformance.ts` |
//!
//! ## Compatibility
//!
//! A JSONL file written by the TypeScript `pi` must load here, and files this
//! crate writes must load there. [`jsonl::codec`] documents the format and
//! `tests/jsonl_wire_compat.rs` pins it with a hand-built fixture.

pub mod compaction;
pub mod context;
pub mod error;
pub mod jsonl;
pub mod memory;
pub mod messages;
pub mod reducer;
pub mod repo;
pub mod search;
pub mod session;
pub mod state;
pub mod testing;
pub mod types;

pub use error::{JsonlDecodeError, JsonlDecodeErrorKind, SessionError, SessionResult};
pub use jsonl::{JsonlSessionRepo, JsonlSessionStorage, JsonlV4Header};
pub use memory::{InMemorySessionRepo, InMemorySessionStorage};
pub use messages::{
    AgentMessage, BashExecutionMessage, BranchSummaryMessage, CompactionSummaryMessage,
    CustomMessage,
};
pub use repo::{
    BranchStore, EntryStore, IdGenerator, SearchBackend, SearchBackendRef, SessionRepo,
    SessionRepoRef, SessionSearchHit, SessionSearchOptions, SessionStorage, SessionStorageRef,
    Uuidv7Generator,
};
pub use session::Session;
pub use state::{SessionMutation, SessionState};
pub use types::*;
