//! Image sniffing for the read tool.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/image.ts`. Detection is
//! by content, never by extension. Animated PNGs and JPEGs with the `0xF7`
//! marker (lossless JPEG, which no provider accepts) are deliberately rejected.

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// The MIME type of a supported image, or `None` for anything else.
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
        return if buffer.get(3) == Some(&0xf7) {
            None
        } else {
            Some("image/jpeg")
        };
    }
    if starts_with(buffer, &PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png")
        } else {
            None
        };
    }
    if starts_with_ascii(buffer, 0, "GIF") {
        return Some("image/gif");
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp");
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}

pub fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_u32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_ascii(buffer, 12, "IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_u32_be(buffer, offset) as usize;
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }
        let Some(next_offset) = offset
            .checked_add(8)
            .and_then(|v| v.checked_add(chunk_length))
            .and_then(|v| v.checked_add(4))
        else {
            return false;
        };
        if next_offset <= offset || next_offset > buffer.len() {
            return false;
        }
        offset = next_offset;
    }
    false
}

fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let declared_file_size = read_u32_le(buffer, 2);
    let pixel_data_offset = read_u32_le(buffer, 10);
    let dib_header_size = read_u32_le(buffer, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if pixel_data_offset < 14 + dib_header_size {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }

    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
    } else {
        return false;
    };
    color_planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
}

fn read_u16_le(buffer: &[u8], offset: usize) -> u32 {
    byte_at(buffer, offset) + (byte_at(buffer, offset + 1) << 8)
}

fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
    byte_at(buffer, offset)
        .wrapping_mul(0x0100_0000)
        .wrapping_add(byte_at(buffer, offset + 1) << 16)
        .wrapping_add(byte_at(buffer, offset + 2) << 8)
        .wrapping_add(byte_at(buffer, offset + 3))
}

fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    byte_at(buffer, offset)
        .wrapping_add(byte_at(buffer, offset + 1) << 8)
        .wrapping_add(byte_at(buffer, offset + 2) << 16)
        .wrapping_add(byte_at(buffer, offset + 3).wrapping_mul(0x0100_0000))
}

fn byte_at(buffer: &[u8], offset: usize) -> u32 {
    buffer.get(offset).copied().unwrap_or(0) as u32
}

fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
    buffer.len() >= bytes.len() && buffer[..bytes.len()] == *bytes
}

fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    let end = offset + text.len();
    buffer.len() >= end && &buffer[offset..end] == text.as_bytes()
}

/// The 1x1 PNG the upstream read-tool test uses.
#[cfg(test)]
pub(crate) fn tiny_png() -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==")
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_bmp() -> Vec<u8> {
        let mut bytes = vec![0u8; 58];
        let size = bytes.len() as u32;
        bytes[0] = 0x42;
        bytes[1] = 0x4d;
        bytes[2..6].copy_from_slice(&size.to_le_bytes());
        bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
        bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&1i32.to_le_bytes());
        bytes[22..26].copy_from_slice(&1i32.to_le_bytes());
        bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
        bytes[34..38].copy_from_slice(&4u32.to_le_bytes());
        bytes
    }

    #[test]
    fn detects_png() {
        assert_eq!(
            detect_supported_image_mime_type(&tiny_png()),
            Some("image/png")
        );
    }

    #[test]
    fn detects_jpeg_but_rejects_lossless_jpeg() {
        assert_eq!(
            detect_supported_image_mime_type(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(
            detect_supported_image_mime_type(&[0xff, 0xd8, 0xff, 0xf7]),
            None
        );
    }

    #[test]
    fn detects_gif_and_webp() {
        assert_eq!(
            detect_supported_image_mime_type(b"GIF89a...."),
            Some("image/gif")
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(detect_supported_image_mime_type(&webp), Some("image/webp"));
    }

    #[test]
    fn detects_bmp() {
        assert_eq!(
            detect_supported_image_mime_type(&tiny_bmp()),
            Some("image/bmp")
        );
    }

    #[test]
    fn rejects_text() {
        assert_eq!(detect_supported_image_mime_type(b"hello world"), None);
        assert_eq!(detect_supported_image_mime_type(b""), None);
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(encode_base64(b"hello"), "aGVsbG8=");
        assert_eq!(encode_base64(b"hi"), "aGk=");
        assert_eq!(encode_base64(b""), "");
    }
}
