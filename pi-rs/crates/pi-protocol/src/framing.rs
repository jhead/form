//! Length framing. Port of `.upstream/packages/protocol/src/framing.ts`.
//!
//! Every frame is an unsigned 32-bit big-endian byte count followed by exactly
//! that many payload bytes. Zero-length frames are legal and carry no payload.

const FRAME_HEADER_LENGTH: usize = 4;

/// Default upper bound for one framed CBOR payload.
pub const DEFAULT_MAX_FRAME_LENGTH: u32 = 16 * 1024 * 1024;

/// Framing limits.
///
/// Upstream throws `RangeError` for a `maxFrameLength` that is negative,
/// fractional, or above `2^32 - 1`; `u32` makes all three unrepresentable, so
/// this port has no equivalent runtime check (or error variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDecoderOptions {
    pub max_frame_length: u32,
}

impl Default for FrameDecoderOptions {
    fn default() -> Self {
        Self {
            max_frame_length: DEFAULT_MAX_FRAME_LENGTH,
        }
    }
}

impl FrameDecoderOptions {
    #[must_use]
    pub fn with_max_frame_length(max_frame_length: u32) -> Self {
        Self { max_frame_length }
    }
}

/// Every failure mode of the framing layer. `Display` text matches upstream's
/// `FrameError` messages verbatim.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("Frame payload exceeds the unsigned 32-bit length limit")]
    PayloadTooLarge,

    #[error("Frame does not contain a complete length prefix")]
    IncompleteLengthPrefix,

    #[error("Frame length {length} exceeds configured limit of {limit}")]
    LengthLimit { length: u32, limit: u32 },

    #[error("Frame must contain exactly one complete payload")]
    NotExactlyOnePayload,

    #[error("Frame decoder has ended")]
    Ended,

    #[error("Frame decoder has failed")]
    Failed,

    #[error("Truncated frame at end of stream")]
    TruncatedStream,
}

impl FrameError {
    /// Stable identifier for FFI consumers.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PayloadTooLarge => "frame_payload_too_large",
            Self::IncompleteLengthPrefix => "frame_incomplete_length_prefix",
            Self::LengthLimit { .. } => "frame_length_limit",
            Self::NotExactlyOnePayload => "frame_not_exactly_one_payload",
            Self::Ended => "frame_decoder_ended",
            Self::Failed => "frame_decoder_failed",
            Self::TruncatedStream => "frame_truncated_stream",
        }
    }
}

/// Prefixes a payload with its unsigned 32-bit big-endian byte length.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::PayloadTooLarge)?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_LENGTH + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Validates that bytes contain exactly one complete frame within the limit.
pub fn assert_complete_frame(frame: &[u8], options: FrameDecoderOptions) -> Result<(), FrameError> {
    if frame.len() < FRAME_HEADER_LENGTH {
        return Err(FrameError::IncompleteLengthPrefix);
    }
    let length = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]);
    if length > options.max_frame_length {
        return Err(FrameError::LengthLimit {
            length,
            limit: options.max_frame_length,
        });
    }
    if frame.len() != FRAME_HEADER_LENGTH + length as usize {
        return Err(FrameError::NotExactlyOnePayload);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderState {
    Open,
    Ended,
    Failed,
}

/// Incrementally splits arbitrary byte chunks into length-prefixed payloads.
#[derive(Debug)]
pub struct FrameDecoder {
    header: [u8; FRAME_HEADER_LENGTH],
    header_length: usize,
    max_frame_length: u32,
    payload: Vec<u8>,
    expected_payload_length: Option<usize>,
    state: DecoderState,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::with_options(FrameDecoderOptions::default())
    }

    pub fn with_options(options: FrameDecoderOptions) -> Self {
        Self {
            header: [0; FRAME_HEADER_LENGTH],
            header_length: 0,
            max_frame_length: options.max_frame_length,
            payload: Vec::new(),
            expected_payload_length: None,
            state: DecoderState::Open,
        }
    }

    /// Feeds a chunk and returns every payload it completed, in order.
    ///
    /// Payload bytes are copied, never borrowed from `chunk`.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        match self.state {
            DecoderState::Ended => return Err(FrameError::Ended),
            DecoderState::Failed => return Err(FrameError::Failed),
            DecoderState::Open => {}
        }

        let mut frames = Vec::new();
        let mut offset = 0;
        while offset < chunk.len() {
            if self.expected_payload_length.is_none() {
                let header_bytes =
                    (FRAME_HEADER_LENGTH - self.header_length).min(chunk.len() - offset);
                self.header[self.header_length..self.header_length + header_bytes]
                    .copy_from_slice(&chunk[offset..offset + header_bytes]);
                self.header_length += header_bytes;
                offset += header_bytes;
                if self.header_length < FRAME_HEADER_LENGTH {
                    continue;
                }

                let frame_length = u32::from_be_bytes(self.header);
                self.header_length = 0;
                if frame_length > self.max_frame_length {
                    let limit = self.max_frame_length;
                    return Err(self.fail(FrameError::LengthLimit {
                        length: frame_length,
                        limit,
                    }));
                }
                if frame_length == 0 {
                    frames.push(Vec::new());
                    continue;
                }
                self.expected_payload_length = Some(frame_length as usize);
                self.payload = Vec::new();
            }

            let expected = self
                .expected_payload_length
                .expect("payload length is set above");
            if offset < chunk.len() && self.payload.len() < expected {
                let remaining = expected - self.payload.len();
                let payload_bytes = remaining.min(chunk.len() - offset);
                // Only ever reserves for bytes actually in hand, so a hostile
                // 16 MiB declared length costs nothing until the bytes arrive.
                // That is the property upstream's 64 KiB block list buys; `Vec`
                // gets it from amortised growth.
                self.payload
                    .extend_from_slice(&chunk[offset..offset + payload_bytes]);
                offset += payload_bytes;
            }
            if self.payload.len() == expected {
                frames.push(std::mem::take(&mut self.payload));
                self.expected_payload_length = None;
            }
        }
        Ok(frames)
    }

    /// Closes the stream. Errors when a frame was left half-read.
    pub fn end(&mut self) -> Result<(), FrameError> {
        match self.state {
            DecoderState::Ended => return Err(FrameError::Ended),
            DecoderState::Failed => return Err(FrameError::Failed),
            DecoderState::Open => {}
        }
        if self.header_length != 0 || self.expected_payload_length.is_some() {
            return Err(self.fail(FrameError::TruncatedStream));
        }
        self.state = DecoderState::Ended;
        Ok(())
    }

    fn fail(&mut self, error: FrameError) -> FrameError {
        self.state = DecoderState::Failed;
        self.header_length = 0;
        self.payload = Vec::new();
        self.expected_payload_length = None;
        error
    }
}
