//! The value model the CBOR subset encodes and decodes.
//!
//! Upstream has no equivalent file: JavaScript's own values *are* the model
//! (`null`, `boolean`, `number`, `string`, `Uint8Array`, arrays, plain objects).
//! Rust needs an explicit enum, so this is the one place where the port adds a
//! type rather than mirroring a file.
//!
//! Two consequences of JavaScript's number model are baked in here and in the
//! encoder, because matching them is what makes the two implementations
//! byte-compatible:
//!
//! 1. JS has no integer/float distinction, so a `Number` that happens to be
//!    integral is written as a CBOR integer. [`CborValue::Float`] therefore
//!    encodes as an integer when its value is integral (see `encoder.rs`).
//! 2. The safe-integer range is `±(2^53 - 1)`, not the full 64-bit CBOR range.

use indexmap::IndexMap;
use serde_json::{Number, Value};

use super::options::MAX_SAFE_INTEGER;

/// An ordered CBOR map. Key order is *significant*: upstream emits map entries
/// in JavaScript property order, never sorted, so the port has to preserve
/// insertion order too.
pub type CborMap = IndexMap<String, CborValue>;

/// One item of the protocol's CBOR subset.
///
/// Tags, indefinite-length items, non-string map keys, `undefined`, and simple
/// values other than `false`/`true`/`null` are deliberately unrepresentable —
/// upstream rejects all of them on both sides of the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum CborValue {
    Null,
    Bool(bool),
    /// A number that is known to be an integer. Must be within the JavaScript
    /// safe-integer range to encode.
    Integer(i64),
    /// A number that is not known to be an integer. Encodes as a CBOR integer
    /// anyway when its value is integral, matching JavaScript.
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Array(Vec<CborValue>),
    Map(CborMap),
}

impl CborValue {
    /// Builds an empty map.
    pub fn map() -> Self {
        Self::Map(CborMap::new())
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[CborValue]> {
        match self {
            Self::Array(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&CborMap> {
        match self {
            Self::Map(value) => Some(value),
            _ => None,
        }
    }

    /// True when the value, or anything nested inside it, is a byte string.
    ///
    /// Upstream's `isProtocolValue` refuses byte strings anywhere in a protocol
    /// message (see the "rejects CBOR byte strings nested in JSON-valued
    /// fields" test); [`CborValue::to_json`] is the mechanism here, and this is
    /// the predicate behind it.
    pub fn contains_bytes(&self) -> bool {
        match self {
            Self::Bytes(_) => true,
            Self::Array(items) => items.iter().any(Self::contains_bytes),
            Self::Map(entries) => entries.values().any(Self::contains_bytes),
            _ => false,
        }
    }

    /// Converts a JSON value into the CBOR model.
    ///
    /// Integral JSON numbers become [`CborValue::Integer`]; everything else
    /// becomes [`CborValue::Float`]. Both encode identically to what upstream
    /// would produce for the same JavaScript value.
    pub fn from_json(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(*value),
            Value::Number(number) => number_to_cbor(number),
            Value::String(value) => Self::Text(value.clone()),
            Value::Array(items) => Self::Array(items.iter().map(Self::from_json).collect()),
            Value::Object(entries) => Self::Map(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::from_json(value)))
                    .collect(),
            ),
        }
    }

    /// Converts to a JSON value, or `None` when the value cannot be one.
    ///
    /// Fails on byte strings (JSON has no such type, and upstream rejects them
    /// in protocol messages) and on non-finite floats.
    ///
    /// A float whose value is integral is narrowed to a JSON integer. That
    /// mirrors JavaScript, where `2` and `2.0` are the same value: a peer that
    /// wrote a timestamp as `f9`/`fb` float64 still deserializes into an
    /// integer-typed schema field, exactly as it would upstream.
    pub fn to_json(&self) -> Option<Value> {
        match self {
            Self::Null => Some(Value::Null),
            Self::Bool(value) => Some(Value::Bool(*value)),
            Self::Integer(value) => Some(Value::Number(Number::from(*value))),
            Self::Float(value) => {
                if !value.is_finite() {
                    return None;
                }
                if is_safe_integral_float(*value) && !is_negative_zero(*value) {
                    return Some(Value::Number(Number::from(*value as i64)));
                }
                Number::from_f64(*value).map(Value::Number)
            }
            Self::Text(value) => Some(Value::String(value.clone())),
            Self::Bytes(_) => None,
            Self::Array(items) => items
                .iter()
                .map(Self::to_json)
                .collect::<Option<Vec<_>>>()
                .map(Value::Array),
            Self::Map(entries) => entries
                .iter()
                .map(|(key, value)| value.to_json().map(|value| (key.clone(), value)))
                .collect::<Option<serde_json::Map<String, Value>>>()
                .map(Value::Object),
        }
    }
}

impl From<Value> for CborValue {
    fn from(value: Value) -> Self {
        Self::from_json(&value)
    }
}

fn number_to_cbor(number: &Number) -> CborValue {
    if let Some(value) = number.as_i64() {
        return CborValue::Integer(value);
    }
    if let Some(value) = number.as_u64() {
        // Beyond i64::MAX, and therefore far beyond the safe-integer range that
        // the encoder accepts; keep it as a float so the encoder reports the
        // same "must be safe JavaScript integers" failure upstream would.
        return CborValue::Float(value as f64);
    }
    CborValue::Float(number.as_f64().unwrap_or(f64::NAN))
}

/// True for a float with no fractional part — JavaScript's `Number.isInteger`.
pub(crate) fn is_integral_float(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0
}

/// True for `-0`, which JavaScript distinguishes from `0` via `Object.is`.
pub(crate) fn is_negative_zero(value: f64) -> bool {
    value == 0.0 && value.is_sign_negative()
}

/// True for an integral float inside the JavaScript safe-integer range —
/// `Number.isSafeInteger`.
pub(crate) fn is_safe_integral_float(value: f64) -> bool {
    is_integral_float(value) && value.abs() <= MAX_SAFE_INTEGER as f64
}
