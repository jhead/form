//! Fast deterministic string hash. Port of `packages/ai/src/utils/hash.ts`.
//!
//! This is cyrb53-style double-hashing rendered as two base-36 numbers. It is
//! **not** cryptographic; it exists to shorten long strings into stable cache
//! keys and ids.
//!
//! The digest has to match the TypeScript implementation exactly, because keys
//! derived from it are written into sessions that both implementations read.
//! That means iterating UTF-16 code units, not `char`s, and reproducing
//! `Math.imul` (a wrapping 32-bit multiply).

/// Shorten a string to a stable 32-bit-pair base-36 digest.
pub fn short_hash(input: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;

    for unit in input.encode_utf16() {
        let ch = unit as u32;
        h1 = (h1 ^ ch).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ ch).wrapping_mul(1_597_334_677);
    }

    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);

    format!("{}{}", to_base36(h2), to_base36(h1))
}

/// `Number.prototype.toString(36)` for an unsigned 32-bit value.
fn to_base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base36_matches_javascript_number_to_string() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
        assert_eq!(to_base36(u32::MAX), "1z141z3");
    }

    #[test]
    fn is_deterministic_and_order_sensitive() {
        assert_eq!(short_hash("hello"), short_hash("hello"));
        assert_ne!(short_hash("hello"), short_hash("olleh"));
        assert_ne!(short_hash(""), short_hash("a"));
    }

    /// Digests produced by running the upstream TypeScript implementation.
    /// Pinned so a refactor cannot silently invalidate keys already on disk.
    #[test]
    fn matches_the_upstream_digests() {
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("a"), "m8735310ae7sx");
        assert_eq!(short_hash("hello"), "1h6qa0qrowduu");
        assert_eq!(short_hash("the quick brown fox"), "116k32etn0sg2");
        assert_eq!(short_hash("anthropic/claude-opus-4-5"), "1etc9tl18ob6pc");
        // An astral-plane char must hash as its two UTF-16 units, not one char.
        assert_eq!(short_hash("🙈"), "kphsz0153ms3q");
    }

    #[test]
    fn produces_short_output() {
        for input in ["", "a", &"x".repeat(100_000)] {
            assert!(short_hash(input).len() <= 14, "{}", short_hash(input));
        }
    }
}
