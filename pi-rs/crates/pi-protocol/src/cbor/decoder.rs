//! Port of `.upstream/packages/protocol/src/cbor/decoder.ts`.
//!
//! Strict by construction: definite lengths only, string map keys only, no
//! duplicate keys, no tags, no `undefined`, no float16/float32, no trailing
//! bytes, and every integer confined to the JavaScript safe range. Declared
//! lengths are checked against the configured limits *before* any bytes are
//! consumed, so a hostile header cannot force an allocation.

use super::options::{CborError, CborItemKind, CborOptions, UINT32_BASE};
use super::value::{CborMap, CborValue};

struct CborReader<'a> {
    bytes: &'a [u8],
    offset: usize,
    options: CborOptions,
}

impl<'a> CborReader<'a> {
    fn new(bytes: &'a [u8], options: CborOptions) -> Self {
        Self {
            bytes,
            offset: 0,
            options,
        }
    }

    fn decode(&mut self) -> Result<CborValue, CborError> {
        let value = self.read_item(0)?;
        if self.offset != self.bytes.len() {
            return Err(CborError::TrailingData);
        }
        Ok(value)
    }

    fn read_item(&mut self, depth: u32) -> Result<CborValue, CborError> {
        if depth > self.options.max_depth {
            return Err(CborError::DepthLimit {
                limit: self.options.max_depth,
            });
        }
        let initial = self.read_byte()?;
        let major_type = initial >> 5;
        let additional_information = initial & 0x1f;

        match major_type {
            0 => {
                let value = self.read_argument(additional_information)?;
                // read_argument already caps at 2^53 - 1, so this cannot wrap.
                Ok(CborValue::Integer(value as i64))
            }
            1 => {
                let argument = self.read_argument(additional_information)?;
                let value = -1i128 - argument as i128;
                if value < super::options::MIN_SAFE_INTEGER as i128 {
                    return Err(CborError::DecodedUnsafeInteger);
                }
                Ok(CborValue::Integer(value as i64))
            }
            2 => {
                let length = self.read_length(
                    additional_information,
                    CborItemKind::ByteString,
                    self.options.max_byte_length,
                )?;
                Ok(CborValue::Bytes(self.read_bytes(length)?.to_vec()))
            }
            3 => {
                let length = self.read_length(
                    additional_information,
                    CborItemKind::TextString,
                    self.options.max_byte_length,
                )?;
                let bytes = self.read_bytes(length)?;
                // `TextDecoder` upstream is constructed with `fatal: true` and
                // `ignoreBOM: true`: overlong forms, surrogates and stray
                // continuation bytes throw, but a leading BOM stays as data.
                match std::str::from_utf8(bytes) {
                    Ok(text) => Ok(CborValue::Text(text.to_owned())),
                    Err(_) => Err(CborError::InvalidUtf8),
                }
            }
            4 => {
                let length = self.read_length(
                    additional_information,
                    CborItemKind::Array,
                    self.options.max_container_length,
                )?;
                let mut items = Vec::new();
                for _ in 0..length {
                    items.push(self.read_item(depth + 1)?);
                }
                Ok(CborValue::Array(items))
            }
            5 => {
                let length = self.read_length(
                    additional_information,
                    CborItemKind::Map,
                    self.options.max_container_length,
                )?;
                let mut entries = CborMap::new();
                for _ in 0..length {
                    let key = match self.read_item(depth + 1)? {
                        CborValue::Text(key) => key,
                        _ => return Err(CborError::NonStringMapKey),
                    };
                    if entries.contains_key(&key) {
                        return Err(CborError::DuplicateMapKey);
                    }
                    let value = self.read_item(depth + 1)?;
                    entries.insert(key, value);
                }
                Ok(CborValue::Map(entries))
            }
            6 => Err(CborError::TagsUnsupported),
            _ => self.read_simple(additional_information),
        }
    }

    fn read_simple(&mut self, additional_information: u8) -> Result<CborValue, CborError> {
        match additional_information {
            20 => Ok(CborValue::Bool(false)),
            21 => Ok(CborValue::Bool(true)),
            22 => Ok(CborValue::Null),
            27 => {
                let bytes = self.read_bytes(8)?;
                let mut buffer = [0u8; 8];
                buffer.copy_from_slice(bytes);
                let value = f64::from_be_bytes(buffer);
                if !value.is_finite() {
                    return Err(CborError::DecodedNonFiniteNumber);
                }
                if super::value::is_integral_float(value)
                    && !super::value::is_safe_integral_float(value)
                {
                    return Err(CborError::DecodedUnsafeInteger);
                }
                Ok(CborValue::Float(value))
            }
            31 => Err(CborError::BreakUnsupported),
            // 23 (`undefined`), 24 (one-byte simple value), 25 (float16) and
            // 26 (float32) all land here; upstream supports none of them.
            _ => Err(CborError::UnsupportedSimpleValue),
        }
    }

    fn read_length(
        &mut self,
        additional_information: u8,
        kind: CborItemKind,
        limit: u32,
    ) -> Result<usize, CborError> {
        if additional_information == 31 {
            return Err(CborError::IndefiniteLength { kind });
        }
        let length = self.read_argument(additional_information)?;
        if length > limit as u64 {
            return Err(CborError::LengthLimit { kind, limit });
        }
        Ok(length as usize)
    }

    fn read_argument(&mut self, additional_information: u8) -> Result<u64, CborError> {
        if additional_information < 24 {
            return Ok(u64::from(additional_information));
        }
        match additional_information {
            24 => Ok(u64::from(self.read_byte()?)),
            25 => {
                let bytes = self.read_bytes(2)?;
                Ok(u64::from(bytes[0]) * 0x100 + u64::from(bytes[1]))
            }
            26 => {
                let bytes = self.read_bytes(4)?;
                Ok(u64::from(bytes[0]) * 0x100_0000
                    + u64::from(bytes[1]) * 0x1_0000
                    + u64::from(bytes[2]) * 0x100
                    + u64::from(bytes[3]))
            }
            27 => {
                let high = self.read_argument(26)?;
                let low = self.read_argument(26)?;
                // Upstream's guard: the top half may not exceed 2^21 - 1, which
                // caps the whole argument at 2^53 - 1.
                if high > 0x1f_ffff {
                    return Err(CborError::DecodedUnsafeArgument);
                }
                Ok(high * UINT32_BASE + low)
            }
            31 => Err(CborError::IndefiniteItem),
            _ => Err(CborError::MalformedAdditionalInformation),
        }
    }

    fn read_byte(&mut self) -> Result<u8, CborError> {
        let value = *self.bytes.get(self.offset).ok_or(CborError::Truncated)?;
        self.offset += 1;
        Ok(value)
    }

    fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], CborError> {
        if length > self.bytes.len() - self.offset {
            return Err(CborError::Truncated);
        }
        let value = &self.bytes[self.offset..self.offset + length];
        self.offset += length;
        Ok(value)
    }
}

/// Decodes exactly one item from the protocol's strict RFC 8949 subset.
pub fn decode_cbor(bytes: &[u8], options: CborOptions) -> Result<CborValue, CborError> {
    let options = options.resolve()?;
    if bytes.len() > options.max_byte_length as usize {
        return Err(CborError::ByteLengthLimit {
            limit: options.max_byte_length,
        });
    }
    CborReader::new(bytes, options).decode()
}
