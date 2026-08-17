//! Wire protocol for the session server: CBOR encoder/decoder, length framing,
//! and the request/response/event schemas.
//!
//! Port of `.upstream/packages/protocol/src/`. A Rust client has to talk to the
//! TypeScript server and vice versa, so **byte-level compatibility with
//! upstream is this crate's entire purpose**. The CBOR codec is hand-rolled for
//! that reason: upstream hand-rolls one too, and its encoding deviates from
//! RFC 8949 canonical form in several places (unsorted map keys, float64-only
//! floats, `-0` as a float, `±(2^53 - 1)` integers). A general-purpose CBOR
//! crate would silently "correct" those and produce different bytes. See
//! [`cbor::encoder`] for the full list.
//!
//! ```
//! use pi_protocol::{
//!     encode_client_message, ClientHello, ClientMessage, ClientMessageDecoder,
//!     FrameDecoderOptions,
//! };
//!
//! let hello = ClientMessage::Hello(ClientHello::default());
//! let frame = encode_client_message(&hello, FrameDecoderOptions::default())?;
//!
//! let mut decoder = ClientMessageDecoder::default();
//! assert_eq!(decoder.push(&frame)?, vec![hello]);
//! decoder.end()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod cbor;
pub mod codec;
pub mod framing;
pub mod schemas;

pub use cbor::{
    decode_cbor, encode_cbor, CborError, CborItemKind, CborMap, CborOptions, CborValue,
    DEFAULT_MAX_CBOR_BYTE_LENGTH, DEFAULT_MAX_CBOR_CONTAINER_LENGTH, DEFAULT_MAX_CBOR_DEPTH,
    MAX_SAFE_INTEGER, MIN_SAFE_INTEGER,
};
pub use codec::{
    encode_client_message, encode_server_message, parse_client_message, parse_client_message_json,
    parse_server_message, parse_server_message_json, ClientMessageDecoder, MessageKind,
    ProtocolValidationError, ServerMessageDecoder,
};
pub use framing::{
    assert_complete_frame, encode_frame, FrameDecoder, FrameDecoderOptions, FrameError,
    DEFAULT_MAX_FRAME_LENGTH,
};
pub use schemas::*;
