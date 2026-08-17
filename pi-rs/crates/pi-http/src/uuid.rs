//! Monotonic UUIDv7.
//!
//! The implementation moved to [`pi_core::uuid`] so that `pi-session`, which
//! depends on id ordering but not on HTTP, can reach it. Re-exported here so
//! existing `pi_http::uuid::…` call sites keep working, and so there is only
//! one implementation to keep byte-compatible with upstream.

pub use pi_core::uuid::{uuidv7, uuidv7_from, UuidV7State};
