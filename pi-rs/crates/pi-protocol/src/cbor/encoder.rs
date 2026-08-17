//! Port of `.upstream/packages/protocol/src/cbor/encoder.ts`.
//!
//! # Where upstream deviates from RFC 8949 canonical CBOR
//!
//! This encoder reproduces upstream byte for byte, including the places where
//! upstream is *not* RFC 8949 §4.2 canonical. Each is marked in the code:
//!
//! * **Map keys are not sorted.** §4.2.1 requires length-then-bytewise ordering.
//!   Upstream emits `Object.keys()` order, i.e. JavaScript property order:
//!   canonical array-index keys ascending first, then the remaining keys in
//!   insertion order. See `js_property_order`.
//! * **Floats are always float64.** §4.2.2 requires the shortest float
//!   representation that round-trips (float16/float32 where possible).
//!   Upstream only ever writes `0xfb`, and its decoder *rejects* float16 and
//!   float32.
//! * **`-0` is written as float64**, not as integer `0`.
//! * **Integers are limited to `±(2^53 - 1)`**, not the full 64-bit range,
//!   because upstream's values are JavaScript numbers.
//!
//! Everything else matches: definite lengths only, shortest-form arguments for
//! integers and lengths, no tags, no indefinite-length items.

use super::options::{
    is_safe_integer, CborError, CborItemKind, CborOptions, MAX_UINT32, UINT32_BASE,
};
use super::value::{
    is_integral_float, is_negative_zero, is_safe_integral_float, CborMap, CborValue,
};

struct CborWriter {
    buffer: Vec<u8>,
    max_byte_length: usize,
}

impl CborWriter {
    fn new(max_byte_length: u32) -> Self {
        let max_byte_length = max_byte_length as usize;
        Self {
            buffer: Vec::with_capacity(max_byte_length.min(256)),
            max_byte_length,
        }
    }

    fn ensure_capacity(&mut self, additional_bytes: usize) -> Result<(), CborError> {
        if self.buffer.len() + additional_bytes > self.max_byte_length {
            return Err(CborError::ByteLengthLimit {
                limit: self.max_byte_length as u32,
            });
        }
        Ok(())
    }

    fn write_byte(&mut self, value: u8) -> Result<(), CborError> {
        self.ensure_capacity(1)?;
        self.buffer.push(value);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CborError> {
        self.ensure_capacity(bytes.len())?;
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Upstream reserves all nine bytes up front, so a payload that would
    /// overflow the limit fails before the `0xfb` head is written.
    fn write_float64(&mut self, value: f64) -> Result<(), CborError> {
        self.ensure_capacity(9)?;
        self.buffer.push(0xfb);
        self.buffer.extend_from_slice(&value.to_be_bytes());
        Ok(())
    }

    fn finish(self) -> Vec<u8> {
        self.buffer
    }
}

/// Shortest-form head for `major_type` carrying `value`, exactly as
/// `writeArgument` does upstream.
fn write_argument(writer: &mut CborWriter, major_type: u8, value: u64) -> Result<(), CborError> {
    let prefix = major_type << 5;
    if value < 24 {
        writer.write_byte(prefix | value as u8)
    } else if value <= 0xff {
        writer.write_byte(prefix | 24)?;
        writer.write_byte(value as u8)
    } else if value <= 0xffff {
        writer.write_byte(prefix | 25)?;
        writer.write_bytes(&(value as u16).to_be_bytes())
    } else if value <= MAX_UINT32 {
        writer.write_byte(prefix | 26)?;
        writer.write_bytes(&(value as u32).to_be_bytes())
    } else {
        writer.write_byte(prefix | 27)?;
        // Upstream splits into two u32 halves; big-endian u64 is the same bytes.
        let high = value / UINT32_BASE;
        let low = value % UINT32_BASE;
        writer.write_bytes(&(high as u32).to_be_bytes())?;
        writer.write_bytes(&(low as u32).to_be_bytes())
    }
}

fn encode_text(
    writer: &mut CborWriter,
    value: &str,
    options: &CborOptions,
) -> Result<(), CborError> {
    // Upstream additionally rejects lone surrogates ("CBOR text strings must
    // contain valid Unicode scalar values"). A Rust `str` cannot hold one, so
    // that check has no representable failure case here.
    let bytes = value.as_bytes();
    if bytes.len() > options.max_byte_length as usize {
        return Err(CborError::LengthLimit {
            kind: CborItemKind::TextString,
            limit: options.max_byte_length,
        });
    }
    write_argument(writer, 3, bytes.len() as u64)?;
    writer.write_bytes(bytes)
}

/// JavaScript `Object.keys()` ordering.
///
/// **Deviation from canonical CBOR, kept on purpose.** V8 (and the spec's
/// `OrdinaryOwnPropertyKeys`) yields canonical array-index keys — decimal
/// strings for integers in `[0, 2^32 - 2]` with no leading zeros — in ascending
/// numeric order first, then every other string key in insertion order. Any
/// object with numeric-looking keys therefore reorders on its way to the wire
/// upstream, and a Rust peer has to reorder identically or the two
/// implementations produce different bytes for the same message.
fn js_property_order(entries: &CborMap) -> Vec<&String> {
    let mut indexed: Vec<(u32, &String)> = Vec::new();
    let mut rest: Vec<&String> = Vec::new();
    for key in entries.keys() {
        match js_array_index(key) {
            Some(index) => indexed.push((index, key)),
            None => rest.push(key),
        }
    }
    indexed.sort_unstable_by_key(|(index, _)| *index);
    indexed
        .into_iter()
        .map(|(_, key)| key)
        .chain(rest)
        .collect()
}

fn js_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    let index: u64 = key.parse().ok()?;
    // 2^32 - 1 is not an array index; the range stops one short.
    if index >= MAX_UINT32 {
        return None;
    }
    Some(index as u32)
}

fn encode_value(
    writer: &mut CborWriter,
    value: &CborValue,
    options: &CborOptions,
    depth: u32,
) -> Result<(), CborError> {
    if depth > options.max_depth {
        return Err(CborError::DepthLimit {
            limit: options.max_depth,
        });
    }

    match value {
        CborValue::Null => writer.write_byte(0xf6),
        CborValue::Bool(true) => writer.write_byte(0xf5),
        CborValue::Bool(false) => writer.write_byte(0xf4),
        CborValue::Integer(value) => encode_integer(writer, *value),
        CborValue::Float(value) => {
            if !value.is_finite() {
                return Err(CborError::NonFiniteNumber);
            }
            // JavaScript has one number type, so an integral value is written as
            // a CBOR integer even when the Rust side calls it a float.
            if is_integral_float(*value) && !is_negative_zero(*value) {
                if !is_safe_integral_float(*value) {
                    return Err(CborError::UnsafeInteger);
                }
                return encode_integer(writer, *value as i64);
            }
            writer.write_float64(*value)
        }
        CborValue::Text(value) => encode_text(writer, value, options),
        CborValue::Bytes(value) => {
            if value.len() > options.max_byte_length as usize {
                return Err(CborError::LengthLimit {
                    kind: CborItemKind::ByteString,
                    limit: options.max_byte_length,
                });
            }
            write_argument(writer, 2, value.len() as u64)?;
            writer.write_bytes(value)
        }
        CborValue::Array(items) => {
            if items.len() > options.max_container_length as usize {
                return Err(CborError::LengthLimit {
                    kind: CborItemKind::Array,
                    limit: options.max_container_length,
                });
            }
            write_argument(writer, 4, items.len() as u64)?;
            for item in items {
                encode_value(writer, item, options, depth + 1)?;
            }
            Ok(())
        }
        CborValue::Map(entries) => {
            if entries.len() > options.max_container_length as usize {
                return Err(CborError::LengthLimit {
                    kind: CborItemKind::Map,
                    limit: options.max_container_length,
                });
            }
            write_argument(writer, 5, entries.len() as u64)?;
            for key in js_property_order(entries) {
                encode_text(writer, key, options)?;
                encode_value(writer, &entries[key], options, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn encode_integer(writer: &mut CborWriter, value: i64) -> Result<(), CborError> {
    if !is_safe_integer(value) {
        return Err(CborError::UnsafeInteger);
    }
    if value >= 0 {
        write_argument(writer, 0, value as u64)
    } else {
        write_argument(writer, 1, (-1 - value) as u64)
    }
}

/// Encodes the protocol's strict, definite-length RFC 8949 subset.
pub fn encode_cbor(value: &CborValue, options: CborOptions) -> Result<Vec<u8>, CborError> {
    let options = options.resolve()?;
    let mut writer = CborWriter::new(options.max_byte_length);
    encode_value(&mut writer, value, &options, 0)?;
    Ok(writer.finish())
}
