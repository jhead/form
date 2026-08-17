//! A minimal PNG writer for the seeded image attachments.
//!
//! The corpus needs *real* image files — Swift rasterizes the thumbnail chips (F3.2, F3.3)
//! from whatever is on disk, so a placeholder byte string would show up as a broken chip on
//! first launch. Rather than pull an encoder in for six demo images, this writes a paletted
//! PNG with stored (uncompressed) deflate blocks: a few dozen lines, no dependency, and
//! small enough to embed in a transcript.

/// Palette PNG, 8-bit indices into a 16-entry ramp. `variant` picks the hue pair.
pub fn encode_gradient(width: u32, height: u32, variant: u8) -> Vec<u8> {
    let palette = ramp(variant);

    // One filter byte (0 = None) per scanline, then one palette index per pixel.
    let mut raw = Vec::with_capacity((height * (width + 1)) as usize);
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            // A diagonal ramp with a soft band, so a thumbnail is recognizably an image.
            let t = (x * 8 / width.max(1) + y * 8 / height.max(1)) as u8;
            raw.push(t.min(15));
        }
    }

    let mut out = Vec::with_capacity(raw.len() + 256);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]); // depth 8, colour type 3 (palette)
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"PLTE", &palette);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn ramp(variant: u8) -> Vec<u8> {
    // (from, to) RGB endpoints; index 0 is the darkest.
    let (from, to): ([i32; 3], [i32; 3]) = match variant % 4 {
        0 => ([18, 24, 38], [94, 166, 235]),
        1 => ([28, 18, 32], [226, 132, 108]),
        2 => ([16, 30, 26], [126, 214, 160]),
        _ => ([26, 22, 16], [232, 198, 112]),
    };
    let mut palette = Vec::with_capacity(48);
    for i in 0..16i32 {
        for c in 0..3 {
            palette.push((from[c] + (to[c] - from[c]) * i / 15).clamp(0, 255) as u8);
        }
    }
    palette
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// zlib stream whose deflate payload is one or more stored (BTYPE=00) blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CMF/FLG for deflate, 32K window, no dictionary
    let mut chunks = data.chunks(0xffff).peekable();
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while let Some(block) = chunks.next() {
        let last = u8::from(chunks.peek().is_none());
        let len = block.len() as u16;
        out.push(last);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_well_formed_png_header_and_terminator() {
        let png = encode_gradient(128, 96, 1);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.ends_with(b"\x00\x00\x00\x00IEND\xae\x42\x60\x82"));
        assert_eq!(
            crate::app::store::image_dimensions(&png),
            Some((128, 96)),
            "IHDR must be parseable by the dimension probe"
        );
        // Paletted 8-bit stays small enough to embed in a transcript.
        assert!(png.len() < 20_000, "{} bytes", png.len());
    }

    #[test]
    fn variants_differ_so_the_corpus_is_not_six_copies() {
        let a = encode_gradient(64, 64, 0);
        let b = encode_gradient(64, 64, 1);
        assert_ne!(a, b);
    }
}
