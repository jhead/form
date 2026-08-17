//! Validation + CBOR + framing, tied together.
//!
//! Port of `.upstream/packages/protocol/src/codec.ts`.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::cbor::{decode_cbor, encode_cbor, CborOptions, CborValue};
use crate::framing::{
    assert_complete_frame, encode_frame, FrameDecoder, FrameDecoderOptions,
    DEFAULT_MAX_FRAME_LENGTH,
};
use crate::schemas::{ClientMessage, JsonValue, ServerMessage, Validate};

/// Which side of the connection a message came from. Only used to reproduce
/// upstream's error wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Client,
    Server,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything the validated codec can reject.
///
/// `Display` reproduces upstream's `ProtocolValidationError` messages. Note in
/// particular that [`ProtocolValidationError::InvalidMessage`] carries no
/// detail at all: upstream deliberately drops the rejected value so an
/// oversized hostile payload cannot end up in a log line, and has a test
/// asserting the message stays short.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolValidationError {
    #[error("Invalid {kind} protocol message")]
    InvalidMessage { kind: MessageKind },

    #[error("Unable to encode {kind} protocol message: {reason}")]
    Encode { kind: MessageKind, reason: String },

    #[error("Invalid {kind} protocol frame: {reason}")]
    InvalidFrame { kind: MessageKind, reason: String },

    #[error("Invalid {kind} protocol framing: {reason}")]
    InvalidFraming { kind: MessageKind, reason: String },

    #[error("{kind} message decoder has failed")]
    DecoderFailed { kind: MessageKind },
}

impl ProtocolValidationError {
    /// Stable identifier for FFI consumers.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidMessage { .. } => "protocol_invalid_message",
            Self::Encode { .. } => "protocol_encode_failed",
            Self::InvalidFrame { .. } => "protocol_invalid_frame",
            Self::InvalidFraming { .. } => "protocol_invalid_framing",
            Self::DecoderFailed { .. } => "protocol_decoder_failed",
        }
    }

    pub fn kind(&self) -> MessageKind {
        match self {
            Self::InvalidMessage { kind }
            | Self::Encode { kind, .. }
            | Self::InvalidFrame { kind, .. }
            | Self::InvalidFraming { kind, .. }
            | Self::DecoderFailed { kind } => *kind,
        }
    }
}

/// Upstream's `boundedErrorMessage`: cap a nested error at 500 characters so a
/// hostile payload cannot inflate the message this crate surfaces.
fn bounded(reason: impl fmt::Display) -> String {
    let reason = reason.to_string();
    if reason.chars().count() <= 500 {
        return reason;
    }
    let truncated: String = reason.chars().take(497).collect();
    format!("{truncated}...")
}

// ---------------------------------------------------------------------------
// parsing
// ---------------------------------------------------------------------------

fn parse_message<T>(value: &CborValue, kind: MessageKind) -> Result<T, ProtocolValidationError>
where
    T: DeserializeOwned + Validate,
{
    // `to_json` returning `None` is this port's `isProtocolValue` check: it
    // fails exactly when the payload holds a CBOR byte string (or a non-finite
    // float), neither of which is a legal protocol value.
    let json = value
        .to_json()
        .ok_or(ProtocolValidationError::InvalidMessage { kind })?;
    parse_message_json(&json, kind)
}

fn parse_message_json<T>(value: &JsonValue, kind: MessageKind) -> Result<T, ProtocolValidationError>
where
    T: DeserializeOwned + Validate,
{
    let message: T =
        T::deserialize(value).map_err(|_| ProtocolValidationError::InvalidMessage { kind })?;
    message
        .validate()
        .map_err(|_| ProtocolValidationError::InvalidMessage { kind })?;
    Ok(message)
}

/// Validates a decoded CBOR value as a client message.
pub fn parse_client_message(value: &CborValue) -> Result<ClientMessage, ProtocolValidationError> {
    parse_message(value, MessageKind::Client)
}

/// Validates a decoded CBOR value as a server message.
pub fn parse_server_message(value: &CborValue) -> Result<ServerMessage, ProtocolValidationError> {
    parse_message(value, MessageKind::Server)
}

/// Validates a JSON value as a client message. JSON is the FFI bridge format,
/// so this is the entry point a Swift caller reaches for.
pub fn parse_client_message_json(
    value: &JsonValue,
) -> Result<ClientMessage, ProtocolValidationError> {
    parse_message_json(value, MessageKind::Client)
}

/// Validates a JSON value as a server message.
pub fn parse_server_message_json(
    value: &JsonValue,
) -> Result<ServerMessage, ProtocolValidationError> {
    parse_message_json(value, MessageKind::Server)
}

// ---------------------------------------------------------------------------
// encoding
// ---------------------------------------------------------------------------

fn encode_protocol_message<T>(
    message: &T,
    kind: MessageKind,
    options: FrameDecoderOptions,
) -> Result<Vec<u8>, ProtocolValidationError>
where
    T: Serialize + Validate,
{
    message
        .validate()
        .map_err(|_| ProtocolValidationError::InvalidMessage { kind })?;

    let json = serde_json::to_value(message).map_err(|error| ProtocolValidationError::Encode {
        kind,
        reason: bounded(error),
    })?;
    // Upstream bounds the CBOR encoder by the frame limit and leaves the
    // container/depth limits at their defaults.
    let payload = encode_cbor(
        &CborValue::from_json(&json),
        CborOptions::default().with_max_byte_length(options.max_frame_length),
    )
    .map_err(|error| ProtocolValidationError::Encode {
        kind,
        reason: bounded(error),
    })?;
    let frame = encode_frame(&payload).map_err(|error| ProtocolValidationError::Encode {
        kind,
        reason: bounded(error),
    })?;
    assert_complete_frame(&frame, options).map_err(|error| ProtocolValidationError::Encode {
        kind,
        reason: bounded(error),
    })?;
    Ok(frame)
}

/// Validates and encodes one complete length-prefixed client message.
pub fn encode_client_message(
    message: &ClientMessage,
    options: FrameDecoderOptions,
) -> Result<Vec<u8>, ProtocolValidationError> {
    encode_protocol_message(message, MessageKind::Client, options)
}

/// Validates and encodes one complete length-prefixed server message.
pub fn encode_server_message(
    message: &ServerMessage,
    options: FrameDecoderOptions,
) -> Result<Vec<u8>, ProtocolValidationError> {
    encode_protocol_message(message, MessageKind::Server, options)
}

// ---------------------------------------------------------------------------
// streaming decoders
// ---------------------------------------------------------------------------

struct ValidatedMessageDecoder<T> {
    failed: bool,
    frames: FrameDecoder,
    kind: MessageKind,
    max_frame_length: u32,
    parse: fn(&CborValue) -> Result<T, ProtocolValidationError>,
}

impl<T> ValidatedMessageDecoder<T> {
    fn new(
        kind: MessageKind,
        parse: fn(&CborValue) -> Result<T, ProtocolValidationError>,
        options: FrameDecoderOptions,
    ) -> Self {
        Self {
            failed: false,
            frames: FrameDecoder::with_options(options),
            kind,
            max_frame_length: options.max_frame_length,
            parse,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<T>, ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError::DecoderFailed { kind: self.kind });
        }
        match self.push_inner(chunk) {
            Ok(messages) => Ok(messages),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn push_inner(&mut self, chunk: &[u8]) -> Result<Vec<T>, ProtocolValidationError> {
        let kind = self.kind;
        let frames =
            self.frames
                .push(chunk)
                .map_err(|error| ProtocolValidationError::InvalidFrame {
                    kind,
                    reason: bounded(error),
                })?;
        let mut messages = Vec::with_capacity(frames.len());
        for frame in frames {
            let value = decode_cbor(
                &frame,
                CborOptions::default().with_max_byte_length(self.max_frame_length),
            )
            .map_err(|error| ProtocolValidationError::InvalidFrame {
                kind,
                reason: bounded(error),
            })?;
            messages.push((self.parse)(&value)?);
        }
        Ok(messages)
    }

    fn end(&mut self) -> Result<(), ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError::DecoderFailed { kind: self.kind });
        }
        let kind = self.kind;
        self.frames.end().map_err(|error| {
            self.failed = true;
            ProtocolValidationError::InvalidFraming {
                kind,
                reason: bounded(error),
            }
        })
    }
}

/// Incrementally decodes and validates framed client messages.
pub struct ClientMessageDecoder {
    decoder: ValidatedMessageDecoder<ClientMessage>,
}

impl Default for ClientMessageDecoder {
    fn default() -> Self {
        Self::new(FrameDecoderOptions::default())
    }
}

impl ClientMessageDecoder {
    pub fn new(options: FrameDecoderOptions) -> Self {
        Self {
            decoder: ValidatedMessageDecoder::new(
                MessageKind::Client,
                parse_client_message,
                options,
            ),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ClientMessage>, ProtocolValidationError> {
        self.decoder.push(chunk)
    }

    pub fn end(&mut self) -> Result<(), ProtocolValidationError> {
        self.decoder.end()
    }
}

/// Incrementally decodes and validates framed server messages.
pub struct ServerMessageDecoder {
    decoder: ValidatedMessageDecoder<ServerMessage>,
}

impl Default for ServerMessageDecoder {
    fn default() -> Self {
        Self::new(FrameDecoderOptions::default())
    }
}

impl ServerMessageDecoder {
    pub fn new(options: FrameDecoderOptions) -> Self {
        Self {
            decoder: ValidatedMessageDecoder::new(
                MessageKind::Server,
                parse_server_message,
                options,
            ),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ServerMessage>, ProtocolValidationError> {
        self.decoder.push(chunk)
    }

    pub fn end(&mut self) -> Result<(), ProtocolValidationError> {
        self.decoder.end()
    }
}

/// The frame limit applied when a caller does not choose one.
pub const DEFAULT_PROTOCOL_FRAME_LENGTH: u32 = DEFAULT_MAX_FRAME_LENGTH;
