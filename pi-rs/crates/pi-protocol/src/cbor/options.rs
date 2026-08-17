//! Limits and errors for the CBOR subset.
//!
//! Port of `.upstream/packages/protocol/src/cbor/options.ts`.

use std::fmt;

/// `2^32`, the divisor upstream uses to split a 64-bit argument into two u32 halves.
pub(crate) const UINT32_BASE: u64 = 0x1_0000_0000;
/// Largest argument encodable in the 4-byte form.
pub(crate) const MAX_UINT32: u64 = 0xffff_ffff;
/// Upstream caps a caller-supplied `maxDepth` here (`MAX_CONFIGURED_DEPTH`).
pub(crate) const MAX_CONFIGURED_DEPTH: u32 = 512;

/// `Number.MAX_SAFE_INTEGER`. Upstream is JavaScript, so the integer range the
/// wire format admits is `±(2^53 - 1)`, not the full CBOR 64-bit range.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
/// `Number.MIN_SAFE_INTEGER`.
pub const MIN_SAFE_INTEGER: i64 = -9_007_199_254_740_991;

/// Safe defaults for untrusted protocol payloads.
pub const DEFAULT_MAX_CBOR_BYTE_LENGTH: u32 = 16 * 1024 * 1024;
/// Default cap on array elements / map entries.
pub const DEFAULT_MAX_CBOR_CONTAINER_LENGTH: u32 = 1_000_000;
/// Default cap on recursive item depth.
pub const DEFAULT_MAX_CBOR_DEPTH: u32 = 64;

/// The kinds of item whose declared length is bounded separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CborItemKind {
    ByteString,
    TextString,
    Array,
    Map,
}

impl CborItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ByteString => "byte string",
            Self::TextString => "text string",
            Self::Array => "array",
            Self::Map => "map",
        }
    }
}

impl fmt::Display for CborItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Encoder/decoder limits.
///
/// Upstream validates these at runtime (`resolveLimit` throws `RangeError` for
/// negative, fractional, or oversized values). Here the unsigned types make
/// every one of those cases unrepresentable except an oversized `max_depth`,
/// which is still checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CborOptions {
    /// Maximum encoded input/output bytes and maximum byte/text string length.
    pub max_byte_length: u32,
    /// Maximum number of elements in an array or entries in a map.
    pub max_container_length: u32,
    /// Maximum recursive item depth. Must not exceed 512.
    pub max_depth: u32,
}

impl Default for CborOptions {
    fn default() -> Self {
        Self {
            max_byte_length: DEFAULT_MAX_CBOR_BYTE_LENGTH,
            max_container_length: DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
            max_depth: DEFAULT_MAX_CBOR_DEPTH,
        }
    }
}

impl CborOptions {
    #[must_use]
    pub fn with_max_byte_length(mut self, max_byte_length: u32) -> Self {
        self.max_byte_length = max_byte_length;
        self
    }

    #[must_use]
    pub fn with_max_container_length(mut self, max_container_length: u32) -> Self {
        self.max_container_length = max_container_length;
        self
    }

    #[must_use]
    pub fn with_max_depth(mut self, max_depth: u32) -> Self {
        self.max_depth = max_depth;
        self
    }

    pub(crate) fn resolve(self) -> Result<Self, CborError> {
        if self.max_depth > MAX_CONFIGURED_DEPTH {
            return Err(CborError::InvalidLimit {
                name: "max_depth",
                maximum: MAX_CONFIGURED_DEPTH,
            });
        }
        Ok(self)
    }
}

/// Every failure mode of the CBOR subset.
///
/// Flat and code-tagged so FFI consumers can match on [`CborError::code`]; the
/// `Display` text is upstream's `CborError` message verbatim, because upstream
/// tests (and this port's) assert on it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CborError {
    #[error("CBOR byte length exceeds configured limit of {limit}")]
    ByteLengthLimit { limit: u32 },

    #[error("CBOR {kind} length exceeds configured limit of {limit}")]
    LengthLimit { kind: CborItemKind, limit: u32 },

    #[error("CBOR nesting depth exceeds configured limit of {limit}")]
    DepthLimit { limit: u32 },

    #[error("CBOR numbers must be finite")]
    NonFiniteNumber,

    #[error("CBOR integers must be safe JavaScript integers")]
    UnsafeInteger,

    #[error("CBOR payload contains trailing data")]
    TrailingData,

    #[error("Truncated CBOR payload")]
    Truncated,

    #[error("Decoded CBOR integer is outside the safe range")]
    DecodedUnsafeInteger,

    #[error("Decoded CBOR integer or length is outside the safe range")]
    DecodedUnsafeArgument,

    #[error("Decoded CBOR number must be finite")]
    DecodedNonFiniteNumber,

    #[error("CBOR text string contains invalid UTF-8")]
    InvalidUtf8,

    #[error("CBOR tags are not supported")]
    TagsUnsupported,

    #[error("CBOR break marker is not supported")]
    BreakUnsupported,

    #[error("Unsupported CBOR simple value or floating-point width")]
    UnsupportedSimpleValue,

    #[error("Indefinite-length CBOR {kind}s are not supported")]
    IndefiniteLength { kind: CborItemKind },

    #[error("Indefinite-length CBOR items are not supported")]
    IndefiniteItem,

    #[error("Malformed CBOR additional information")]
    MalformedAdditionalInformation,

    #[error("CBOR map keys must be strings")]
    NonStringMapKey,

    #[error("CBOR map contains a duplicate key")]
    DuplicateMapKey,

    #[error("{name} must be an integer between 0 and {maximum}")]
    InvalidLimit { name: &'static str, maximum: u32 },
}

impl CborError {
    /// Stable identifier for FFI consumers.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ByteLengthLimit { .. } => "cbor_byte_length_limit",
            Self::LengthLimit { .. } => "cbor_length_limit",
            Self::DepthLimit { .. } => "cbor_depth_limit",
            Self::NonFiniteNumber => "cbor_non_finite_number",
            Self::UnsafeInteger => "cbor_unsafe_integer",
            Self::TrailingData => "cbor_trailing_data",
            Self::Truncated => "cbor_truncated",
            Self::DecodedUnsafeInteger => "cbor_decoded_unsafe_integer",
            Self::DecodedUnsafeArgument => "cbor_decoded_unsafe_argument",
            Self::DecodedNonFiniteNumber => "cbor_decoded_non_finite_number",
            Self::InvalidUtf8 => "cbor_invalid_utf8",
            Self::TagsUnsupported => "cbor_tags_unsupported",
            Self::BreakUnsupported => "cbor_break_unsupported",
            Self::UnsupportedSimpleValue => "cbor_unsupported_simple_value",
            Self::IndefiniteLength { .. } => "cbor_indefinite_length",
            Self::IndefiniteItem => "cbor_indefinite_item",
            Self::MalformedAdditionalInformation => "cbor_malformed_additional_information",
            Self::NonStringMapKey => "cbor_non_string_map_key",
            Self::DuplicateMapKey => "cbor_duplicate_map_key",
            Self::InvalidLimit { .. } => "cbor_invalid_limit",
        }
    }
}

/// True for values JavaScript would accept as a safe integer.
pub(crate) fn is_safe_integer(value: i64) -> bool {
    (MIN_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value)
}
