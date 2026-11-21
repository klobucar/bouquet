//! Default keyed hasher.
//!
//! `DefaultHasher` is a thin wrapper around SipHash-1-3 that holds a 128-bit
//! key. Two filters created with `DefaultHasher::new()` will *not* hash
//! identically — that is intentional, and is what makes the default
//! HashDoS-resistant. If you need bit-for-bit reproducibility (e.g. for
//! persistence), construct one explicitly with `DefaultHasher::with_seed`.

use core::hash::BuildHasher;
use siphasher::sip::SipHasher13;

/// Keyed SipHash-1-3 `BuildHasher`.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DefaultHasher {
    key0: u64,
    key1: u64,
}

impl DefaultHasher {
    /// Construct a hasher with a fresh random key.
    ///
    /// Available only with the `std` feature; without `std` there is no
    /// portable source of randomness, and you must pick a seed yourself with
    /// [`Self::with_seed`].
    #[cfg(feature = "std")]
    pub fn new() -> Self {
        use std::collections::hash_map::RandomState;
        // RandomState seeds itself from the OS RNG. Hashing two distinct
        // sentinel values gives us 128 bits of entropy without pulling in
        // `getrandom` directly.
        let rs = RandomState::new();
        let key0 = rs.hash_one(0x0000_0b10_075eu64);
        let key1 = rs.hash_one(0xfee1_dead_beefu64);
        Self { key0, key1 }
    }

    /// Construct a hasher from an explicit 128-bit seed. Two hashers with the
    /// same seed will hash identically forever.
    pub const fn with_seed(seed: u128) -> Self {
        Self {
            key0: seed as u64,
            key1: (seed >> 64) as u64,
        }
    }

    /// The seed used by this hasher, as a single 128-bit value.
    pub const fn seed(&self) -> u128 {
        ((self.key1 as u128) << 64) | (self.key0 as u128)
    }
}

#[cfg(feature = "std")]
impl Default for DefaultHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Redacts the key. The key is what makes the default hasher HashDoS
/// resistant, and `Filter` and the builders embed the hasher in their own
/// `Debug` output, so a derived impl would leak it into any `{:?}` log line.
impl core::fmt::Debug for DefaultHasher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DefaultHasher").finish_non_exhaustive()
    }
}

impl PartialEq for DefaultHasher {
    fn eq(&self, other: &Self) -> bool {
        self.key0 == other.key0 && self.key1 == other.key1
    }
}

impl Eq for DefaultHasher {}

impl BuildHasher for DefaultHasher {
    type Hasher = SipHasher13;

    fn build_hasher(&self) -> Self::Hasher {
        SipHasher13::new_with_keys(self.key0, self.key1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::hash::Hasher;

    #[test]
    fn same_seed_hashes_identically() {
        let a = DefaultHasher::with_seed(0xdead_beef);
        let b = DefaultHasher::with_seed(0xdead_beef);

        let mut ha = a.build_hasher();
        let mut hb = b.build_hasher();
        ha.write(b"hello world");
        hb.write(b"hello world");
        assert_eq!(ha.finish(), hb.finish());
    }

    #[test]
    fn different_seed_diverges() {
        let a = DefaultHasher::with_seed(1);
        let b = DefaultHasher::with_seed(2);

        let mut ha = a.build_hasher();
        let mut hb = b.build_hasher();
        ha.write(b"same input");
        hb.write(b"same input");
        assert_ne!(ha.finish(), hb.finish());
    }

    #[cfg(feature = "std")]
    #[test]
    fn new_is_random() {
        // Vanishingly unlikely to collide.
        let a = DefaultHasher::new();
        let b = DefaultHasher::new();
        assert_ne!(a.seed(), b.seed());
    }

    #[test]
    fn debug_does_not_leak_key() {
        extern crate alloc;
        let h = DefaultHasher::with_seed(0xfeed_face_dead_beef_cafe_babe_1234_5678);
        assert_eq!(alloc::format!("{h:?}"), "DefaultHasher { .. }");
    }

    #[test]
    fn seed_round_trips() {
        let s = 0xfeed_face_dead_beef_cafe_babe_1234_5678u128;
        let h = DefaultHasher::with_seed(s);
        assert_eq!(h.seed(), s);
    }
}
