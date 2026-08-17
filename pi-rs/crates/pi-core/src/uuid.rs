//! Monotonic UUIDv7. Port of `packages/ai/src/utils/uuid.ts`.
//!
//! Ids generated here sort in creation order, which the session layer relies on
//! to order entries written inside the same millisecond. That requires a
//! *counter*, not just randomness: upstream seeds a 32-bit sequence from random
//! bytes on the first id of a millisecond and increments it for every id after,
//! bumping the timestamp when the sequence wraps. This reproduces that byte
//! layout exactly (RFC 9562 §5.7, "method 3" replacing rand_a/rand_b).
//!
//! The `uuid` crate's `now_v7` is *not* monotonic within a millisecond, which is
//! why this lives here rather than delegating: two entries appended in the same
//! tick would otherwise sort arbitrarily.

use std::sync::{LazyLock, Mutex};

use crate::message::now_ms;

/// Sequence state. Exposed so the layout can be tested with fixed inputs.
#[derive(Debug, Clone, Copy)]
pub struct UuidV7State {
    last_timestamp: i64,
    sequence: u32,
}

impl Default for UuidV7State {
    fn default() -> Self {
        // Upstream starts at -Infinity so the first call always takes the
        // "new millisecond" branch.
        Self {
            last_timestamp: i64::MIN,
            sequence: 0,
        }
    }
}

static STATE: LazyLock<Mutex<UuidV7State>> = LazyLock::new(|| Mutex::new(UuidV7State::default()));

/// Generate a time-ordered UUIDv7. Monotonic across threads.
pub fn uuidv7() -> String {
    let mut random = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut random);
    let timestamp = now_ms();
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    uuidv7_from(&mut state, timestamp, random)
}

/// The pure core: derive the next id from explicit state, clock and randomness.
pub fn uuidv7_from(state: &mut UuidV7State, timestamp_ms: i64, random: [u8; 16]) -> String {
    if timestamp_ms > state.last_timestamp {
        state.sequence = u32::from_be_bytes([random[6], random[7], random[8], random[9]]);
        state.last_timestamp = timestamp_ms;
    } else {
        state.sequence = state.sequence.wrapping_add(1);
        if state.sequence == 0 {
            // Sequence exhausted inside one millisecond: borrow from the future
            // rather than emit a non-monotonic id.
            state.last_timestamp += 1;
        }
    }

    let ts = state.last_timestamp as u64;
    let sequence = state.sequence;
    let mut bytes = [0u8; 16];
    bytes[0] = (ts >> 40) as u8;
    bytes[1] = (ts >> 32) as u8;
    bytes[2] = (ts >> 24) as u8;
    bytes[3] = (ts >> 16) as u8;
    bytes[4] = (ts >> 8) as u8;
    bytes[5] = ts as u8;
    bytes[6] = 0x70 | ((sequence >> 28) & 0x0f) as u8; // version 7
    bytes[7] = ((sequence >> 20) & 0xff) as u8;
    bytes[8] = 0x80 | ((sequence >> 14) & 0x3f) as u8; // variant 10
    bytes[9] = ((sequence >> 6) & 0xff) as u8;
    bytes[10] = (((sequence & 0x3f) << 2) as u8) | (random[10] & 0x03);
    bytes[11..16].copy_from_slice(&random[11..16]);

    format_uuid(&bytes)
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMESTAMP: i64 = 0x0123_4567_89ab;

    fn is_uuid_v7(text: &str) -> bool {
        let parts: Vec<&str> = text.split('-').collect();
        parts.len() == 5
            && [8, 4, 4, 4, 12]
                == [
                    parts[0].len(),
                    parts[1].len(),
                    parts[2].len(),
                    parts[3].len(),
                    parts[4].len(),
                ]
            && text.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
            && parts[2].starts_with('7')
            && matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b')
    }

    /// Upstream `uuid.test.ts`: exact byte layout plus monotonic rollover.
    #[test]
    fn uses_the_rfc_9562_layout_and_preserves_monotonic_order() {
        let mut state = UuidV7State::default();
        let mut random = [0u8; 16];
        random[6..10].copy_from_slice(&[0xff, 0xff, 0xff, 0xfe]);
        random[10..16].copy_from_slice(&[0x01, 0x11, 0x22, 0x33, 0x44, 0x55]);

        let first = uuidv7_from(&mut state, TIMESTAMP, random);
        let second = uuidv7_from(&mut state, TIMESTAMP, [0u8; 16]);
        let third = uuidv7_from(&mut state, TIMESTAMP, [0u8; 16]);

        assert_eq!(first, "01234567-89ab-7fff-bfff-f91122334455");
        assert_eq!(second, "01234567-89ab-7fff-bfff-fc0000000000");
        // The sequence wrapped, so the timestamp advanced by one.
        assert_eq!(third, "01234567-89ac-7000-8000-000000000000");

        for id in [&first, &second, &third] {
            assert!(is_uuid_v7(id), "{id}");
        }
        assert!(first < second);
        assert!(second < third);
    }

    #[test]
    fn encodes_the_timestamp_in_the_first_48_bits() {
        let mut state = UuidV7State::default();
        let id = uuidv7_from(&mut state, TIMESTAMP, [0u8; 16]);
        let hex: String = id.replace('-', "").chars().take(12).collect();
        assert_eq!(i64::from_str_radix(&hex, 16).unwrap(), TIMESTAMP);
    }

    #[test]
    fn a_new_millisecond_reseeds_the_sequence_from_randomness() {
        let mut state = UuidV7State::default();
        let mut random = [0u8; 16];
        random[6..10].copy_from_slice(&[0x00, 0x00, 0x00, 0x05]);
        uuidv7_from(&mut state, TIMESTAMP, random);
        assert_eq!(state.sequence, 5);
        random[6..10].copy_from_slice(&[0x00, 0x00, 0x00, 0x09]);
        uuidv7_from(&mut state, TIMESTAMP + 1, random);
        assert_eq!(state.sequence, 9);
    }

    #[test]
    fn a_clock_that_goes_backwards_still_increments() {
        let mut state = UuidV7State::default();
        let a = uuidv7_from(&mut state, TIMESTAMP, [0u8; 16]);
        let b = uuidv7_from(&mut state, TIMESTAMP - 5_000, [0u8; 16]);
        assert!(b > a, "{a} then {b}");
    }

    #[test]
    fn generated_ids_are_well_formed_and_strictly_increasing() {
        let ids: Vec<String> = (0..1_000).map(|_| uuidv7()).collect();
        for id in &ids {
            assert!(is_uuid_v7(id), "{id}");
        }
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "ids are not monotonic");
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "ids are not unique");
    }
}
