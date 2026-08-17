//! The protocol's hand-rolled CBOR subset.
//!
//! Port of `.upstream/packages/protocol/src/cbor/`. Hand-rolled here for the
//! same reason it is hand-rolled upstream: the encoding is a deliberately small,
//! strict, JavaScript-shaped subset of RFC 8949, and it deviates from canonical
//! CBOR in ways a general-purpose crate would "fix" (see `encoder.rs`).

pub mod decoder;
pub mod encoder;
pub mod options;
pub mod value;

pub use decoder::decode_cbor;
pub use encoder::encode_cbor;
pub use options::{
    CborError, CborItemKind, CborOptions, DEFAULT_MAX_CBOR_BYTE_LENGTH,
    DEFAULT_MAX_CBOR_CONTAINER_LENGTH, DEFAULT_MAX_CBOR_DEPTH, MAX_SAFE_INTEGER, MIN_SAFE_INTEGER,
};
pub use value::{CborMap, CborValue};
