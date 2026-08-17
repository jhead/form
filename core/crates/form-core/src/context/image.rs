//! Per-image context cost, derived from the image's dimensions.
//!
//! Attachments do not arrive with a token count, so the ring has to derive one. The formula
//! is the provider-agnostic version of what the vision APIs document: downscale to the
//! largest size the provider will accept, then charge by area.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

/// Longest edge a provider keeps before it downscales for you.
pub const MAX_EDGE: u32 = 1568;
/// Area cap after downscaling, roughly 1.15 megapixels.
pub const MAX_PIXELS: f64 = 1_150_000.0;
/// Pixels per token once the image has been fitted.
pub const PIXELS_PER_TOKEN: f64 = 750.0;
/// Even a favicon costs a tile.
pub const MIN_IMAGE_TOKENS: u64 = 85;
/// What an image whose dimensions we cannot read costs: a 1024 × 1024 screenshot, the shape
/// of the overwhelming majority of pasted images.
pub const UNKNOWN_IMAGE_TOKENS: u64 = 1_400;

/// Only the header is needed, and a transcript image can be several megabytes.
const HEADER_SCAN_BYTES: usize = 64 * 1024;

pub fn image_tokens(width: u32, height: u32) -> u64 {
    if width == 0 || height == 0 {
        return UNKNOWN_IMAGE_TOKENS;
    }
    let (w, h) = (width as f64, height as f64);
    let scale = (MAX_EDGE as f64 / w.max(h)).min(1.0);
    let pixels = (w * scale * h * scale).min(MAX_PIXELS);
    ((pixels / PIXELS_PER_TOKEN).ceil() as u64).max(MIN_IMAGE_TOKENS)
}

/// Cost of an image we only know by its inline base64 payload.
pub fn tokens_for_base64(data: &str) -> u64 {
    match dimensions_from_base64(data) {
        Some((w, h)) => image_tokens(w, h),
        None => UNKNOWN_IMAGE_TOKENS,
    }
}

/// Decode just enough of the payload to read a header. Accepts a bare base64 string or a
/// `data:` URL, and tolerates the line breaks some encoders insert.
pub fn dimensions_from_base64(data: &str) -> Option<(u32, u32)> {
    let payload = match data.find(";base64,") {
        Some(i) => &data[i + ";base64,".len()..],
        None => data,
    };
    let mut compact = String::with_capacity(payload.len().min(HEADER_SCAN_BYTES));
    for c in payload.chars() {
        if c.is_whitespace() {
            continue;
        }
        compact.push(c);
        if compact.len() >= HEADER_SCAN_BYTES {
            break;
        }
    }
    // Base64 decodes in 4-character groups; truncate to a group boundary so a prefix of a
    // longer payload still decodes.
    compact.truncate(compact.len() - compact.len() % 4);
    let bytes = STANDARD.decode(compact.as_bytes()).ok()?;
    dimensions(&bytes)
}

/// Width and height from an image header. PNG, JPEG, GIF, BMP and WebP cover everything the
/// attachment intake accepts (spec 13, Part B).
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        // IHDR is always the first chunk: 8 signature + 4 length + 4 type, then w, h.
        return Some((be_u32(bytes, 16)?, be_u32(bytes, 20)?));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some((le_u16(bytes, 6)? as u32, le_u16(bytes, 8)? as u32));
    }
    if bytes.starts_with(b"BM") {
        // BITMAPINFOHEADER stores height signed; a bottom-up bitmap reports it negative.
        let w = le_u32(bytes, 18)? as i32;
        let h = le_u32(bytes, 22)? as i32;
        return Some((w.unsigned_abs(), h.unsigned_abs()));
    }
    if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
        return webp_dimensions(bytes);
    }
    if bytes.starts_with(b"\xff\xd8") {
        return jpeg_dimensions(bytes);
    }
    None
}

/// Walk the JPEG segment chain to the frame header, which is the only place the size lives.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        // Standalone markers carry no length.
        if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            i += 2;
            continue;
        }
        let length = be_u16(bytes, i + 2)? as usize;
        // SOF0..SOF15, excluding the DHT/JPG/DAC markers interleaved in that range.
        let is_frame =
            (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc;
        if is_frame {
            let height = be_u16(bytes, i + 5)? as u32;
            let width = be_u16(bytes, i + 7)? as u32;
            return Some((width, height));
        }
        if length < 2 {
            return None;
        }
        i += 2 + length;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match bytes.get(12..16)? {
        b"VP8X" => {
            // 24-bit little-endian, stored as (dimension - 1).
            let w = le_u24(bytes, 24)? + 1;
            let h = le_u24(bytes, 27)? + 1;
            Some((w, h))
        }
        b"VP8 " => {
            // Key frame header: 3-byte tag, 3-byte start code, then 14-bit dimensions.
            let w = (le_u16(bytes, 26)? & 0x3fff) as u32;
            let h = (le_u16(bytes, 28)? & 0x3fff) as u32;
            Some((w, h))
        }
        b"VP8L" => {
            let bits = le_u32(bytes, 21)?;
            let w = (bits & 0x3fff) + 1;
            let h = ((bits >> 14) & 0x3fff) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le_u24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
