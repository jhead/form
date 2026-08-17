//! The append-only JSONL v4 session store.
//!
//! Port of `harness/session/jsonl/`. [`codec`] owns the wire format and is the
//! module to read first: it is the compatibility contract with the TypeScript
//! implementation.

pub mod codec;
pub mod repo;
pub mod storage;

pub use codec::{
    encode_header, encode_mutation, metadata_from_header, parse_header, parse_mutation,
    JsonlV4Header,
};
pub use repo::{list_jsonl_session_metadata, JsonlSessionRepo};
pub use storage::JsonlSessionStorage;
