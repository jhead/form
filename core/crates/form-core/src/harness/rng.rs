//! Deterministic seeding.
//!
//! The same key must always produce the same transcript — stable screenshots and snapshot
//! tests depend on it. That rules out `DefaultHasher`, whose output is explicitly allowed to
//! change between compiler releases, so the mixing function is written out here and is part
//! of the contract.

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub(super) fn seed(key: &str, turn_index: u32) -> u64 {
    let turn = turn_index.to_le_bytes();
    let mut hash = FNV_OFFSET;
    for byte in key.as_bytes().iter().chain(turn.iter()) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

pub(super) fn rng(key: &str, turn_index: u32) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed(key, turn_index))
}

/// Pick one element, deterministically. Panics on an empty slice, which would be a bug in
/// the corpus rather than a runtime condition.
pub(super) fn pick<'a, T>(rng: &mut ChaCha8Rng, items: &'a [T]) -> &'a T {
    use rand::Rng;
    &items[rng.gen_range(0..items.len())]
}
