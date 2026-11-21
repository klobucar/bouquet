//! Backend trait + index derivation.
//!
//! A *backend* owns the bit storage of a Bloom filter and the hot
//! `set_item` / `check_item` loops. Filter dispatch is generic; there is no
//! `dyn` indirection. Each backend lives behind its own cargo feature so
//! that a user who only needs `Classical` does not pay compile-time cost
//! for `Atomic` (and vice-versa).
//!
//! ## One trait, interior mutability
//!
//! Every backend mutates through `&self`:
//!
//! - `Classical` and the two blocked backends wrap each word in
//!   [`core::cell::Cell`], which is `!Sync`. `&Filter<Classical>` therefore
//!   cannot cross thread boundaries; the borrow checker prevents misuse
//!   without anyone having to remember a rule.
//! - `Atomic` wraps each word in [`core::sync::atomic::AtomicU64`], which is
//!   `Sync`. `&Filter<Atomic>` is freely shareable across threads.
//!
//! Note that `&self` is *not* what distinguishes the concurrent backend —
//! `insert` takes `&self` for all four. `Sync` is the distinction.
//!
//! ## Layouts
//!
//! Backends fall into families that share a bit layout *and* a probe
//! scheme; see [`Layout`]. Two backends in the same family can read each
//! other's serialized bits; two in different families cannot, and the
//! deserializer enforces that.
//!
//! Sticking with one trait keeps the public `Filter::insert` signature
//! identical for both backends — pick the backend, the API does not change.
//! The variant the brief considered (a `BackendMut` / `BackendConcurrent`
//! split) was rejected because two `insert` methods on `Filter<B>` would
//! collide under coherence rules and the user-facing surface would have to
//! diverge.

use core::fmt;

#[cfg(feature = "atomic")]
pub mod atomic;
#[cfg(feature = "blocked")]
pub mod blocked;
#[cfg(feature = "classical")]
pub mod classical;
#[cfg(feature = "simd")]
pub mod simd_blocked;

#[cfg(feature = "atomic")]
pub use atomic::Atomic;
#[cfg(feature = "blocked")]
pub use blocked::Blocked;
#[cfg(feature = "classical")]
pub use classical::Classical;
#[cfg(feature = "simd")]
pub use simd_blocked::SimdBlocked;

/// How a backend arranges its bits and derives probe positions.
///
/// This is the compatibility key for serialized filters. Backends sharing a
/// `Layout` produce byte-identical bits for the same inserts and can load
/// each other's blobs; backends with different `Layout`s cannot, because
/// the same word array means something different to each.
///
/// The tag is written into the serialized form and checked on load. Without
/// it a `Filter<Classical>` blob deserializes *successfully* as a
/// `Filter<Blocked>` — the word count satisfies both layouts' checks — and
/// then returns false negatives for every item it contains, which defeats
/// the one guarantee a Bloom filter makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Layout {
    /// Flat bit array; an item's probes scatter across the whole filter.
    /// Used by `Classical` and `Atomic`.
    Flat,
    /// 512-bit blocks; an item's probes are confined to one block. Used by
    /// `Blocked` and `SimdBlocked`.
    Blocked512,
}

impl Layout {
    /// Stable wire tag. Changing one of these strings is a format break.
    pub const fn as_str(self) -> &'static str {
        match self {
            Layout::Flat => "flat",
            Layout::Blocked512 => "blocked512",
        }
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A Bloom filter backend.
///
/// Backends own the hot loop. The `Filter` provides the seed hash; the
/// backend decides how to translate that into bit positions and how to
/// touch them. Concrete backends are expected to inline aggressively.
pub trait Backend: Sized {
    /// This backend's bit layout and probe scheme. See [`Layout`].
    const LAYOUT: Layout;

    /// Clamp a requested `k` to what this backend can usefully use.
    ///
    /// Applied once at construction (and on deserialize) so that
    /// [`crate::Filter::num_hashes`] reports the value the probe loops
    /// actually run, rather than a request that was silently ignored.
    ///
    /// The default is the identity — a flat filter can use any `k` up to
    /// [`crate::math::MAX_HASHES`]. The blocked backends override it,
    /// because every probe for an item lands in one 512-bit block and a
    /// large `k` there saturates the block instead of sharpening the
    /// filter.
    fn clamp_hashes(k: u32) -> u32 {
        k
    }

    /// Construct backend storage of *at least* `num_bits` bits, all zero.
    ///
    /// Implementations round `num_bits` up to their layout granularity (a
    /// 64-bit word, or a whole block) and floor at
    /// [`crate::math::MIN_BITS`]; [`Self::num_bits`] reports the rounded
    /// value. The rounding is unconditional — the hot loops use unchecked
    /// indexing that relies on `num_bits() == words * 64`, and this method
    /// is publicly callable with arbitrary sizes, so the invariant cannot
    /// be left to the `Filter` builder alone.
    ///
    /// # Panics
    ///
    /// Panics if `num_bits` is too large to round up without overflowing
    /// `usize`, and **aborts the process** if the allocation fails — Rust's
    /// global allocator calls `handle_alloc_error`, which is not
    /// catchable. Use [`Self::try_with_capacity`] when the size comes from
    /// untrusted input.
    fn with_capacity(num_bits: usize) -> Self;

    /// Fallible [`Self::with_capacity`].
    ///
    /// Reserves through `Vec::try_reserve_exact`, so an allocation the
    /// system cannot satisfy comes back as
    /// [`crate::Error::AllocationFailed`] instead of aborting. This is the
    /// constructor to use for any size derived from user input — a
    /// deserialized blob, a config file, or a language binding's argument.
    fn try_with_capacity(num_bits: usize) -> Result<Self, crate::Error>;

    /// Total bit capacity. Always a multiple of 64.
    fn num_bits(&self) -> usize;

    /// Set every probe for `hash`. Returns `true` if all `k` bits were
    /// already set (i.e. this was a redundant insert).
    ///
    /// ## Concurrency
    ///
    /// For `Atomic`, the return value is **not** a mutual-exclusion
    /// primitive. A thread that flips a bit always observes
    /// `prev & mask == 0` from its own `fetch_or` and so returns `false`, so
    /// concurrent calls cannot *both* report "already set". The reverse can
    /// and does happen: two threads inserting the *same* item may each win
    /// the race on a different bit, and then **both** return `false`
    /// ("newly inserted"). Do not use it to elect a single winner or to
    /// deduplicate work across threads.
    fn set_item(&self, hash: u64, k: u32) -> bool;

    /// Test every probe for `hash`. Returns `true` only if every probe
    /// finds a set bit.
    fn check_item(&self, hash: u64, k: u32) -> bool;

    /// Number of set bits across the whole filter.
    fn count_ones(&self) -> usize;

    /// Reset every bit to zero. Takes `&mut self` so that "clear" is, by
    /// construction, not racy with concurrent `set_item` / `check_item`.
    fn clear(&mut self);

    /// Snapshot the storage as a `Vec<u64>`. Used by the serde implementation.
    /// Concurrent backends are expected to use a relaxed load — a snapshot
    /// taken concurrently with inserts may lag, but every observed bit is
    /// either correct-as-of-some-call or a bit that was about to be set.
    #[cfg(feature = "serde")]
    fn snapshot_words(&self) -> alloc::vec::Vec<u64>;

    /// Reconstruct backend storage from a word array. Returns `None` if
    /// `words` is empty (the deserializer rejects this with a typed error).
    #[cfg(feature = "serde")]
    fn from_words(words: alloc::vec::Vec<u64>) -> Option<Self>;
}

/// Round `num_bits` up to a whole number of `granularity`-bit units, floored
/// at one unit.
///
/// The rounding is what makes `num_bits() == words * 64` (and, for blocked
/// layouts, `num_blocks * BLOCK_WORDS == words`) an invariant the hot loops
/// can rely on for unchecked indexing.
pub(crate) fn round_up_to(num_bits: usize, granularity: usize) -> Result<usize, crate::Error> {
    num_bits
        .div_ceil(granularity)
        .checked_mul(granularity)
        .map(|rounded| rounded.max(granularity))
        .ok_or(crate::Error::CapacityOverflow {
            requested_bits: num_bits,
        })
}

/// Allocate `words` zeroed `u64`-sized cells, reporting allocator failure
/// rather than aborting.
///
/// Note this deliberately does *not* use `vec![zero; n]`. That form is only
/// lowered to `calloc` for element types the standard library can prove are
/// all-zero-bytes (`IsZero`), which `Cell<u64>` and `AtomicU64` are not — so
/// the macro clone-fills and touches every page anyway. Since there is no
/// calloc fast path to lose, we may as well take the fallible allocation.
pub(crate) fn try_alloc_words<T, F>(
    words: usize,
    num_bits: usize,
    mut zero: F,
) -> Result<alloc::vec::Vec<T>, crate::Error>
where
    F: FnMut() -> T,
{
    let mut v: alloc::vec::Vec<T> = alloc::vec::Vec::new();
    v.try_reserve_exact(words)
        .map_err(|_| crate::Error::AllocationFailed { bits: num_bits })?;
    v.resize_with(words, &mut zero);
    Ok(v)
}

// ---------------------------------------------------------------------------
// Index derivation, shared by every backend.
// ---------------------------------------------------------------------------

/// Compute the `(h1, h2)` pair used by double-hashing.
///
/// `h1` is the seed hash unchanged. `h2` is derived through a SplitMix64-style
/// finalizer of `h1`, then ORed with `1`. This guarantees:
///
/// - For *any* input including `hash == 0` and `hash == u64::MAX`, `h2` is a
///   well-distributed 64-bit value, never tiny.
/// - `h2` is always odd. Note this is *not* about coprimality with
///   `num_bits`: [`nth_index`] uses multiplicative fastrange, which reads
///   the high bits of the product and never takes a modulo, so there is no
///   modulus to be coprime with. Odd `h2` matters only in that it keeps
///   successive `h1 + i·h2` values from sharing low-order structure.
///
/// Carrying the "odd stride" habit into a scheme where the index *is* a
/// modulo is what produced an early blocked-backend false-positive floor;
/// see [`blocked`] for that story.
///
/// Some Bloom filter implementations have shipped derivations with a fixed
/// point at zero, collapsing every probe to bit 0. The unit tests below
/// verify we do not have that bug.
#[inline(always)]
pub fn double_hash(hash: u64) -> (u64, u64) {
    let mut z = hash.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let h2 = (z ^ (z >> 31)) | 1;
    (hash, h2)
}

/// `i`-th probe index. Replaces `(h1 + i*h2) % num_bits` with the
/// multiplicative fast-range
/// `((h1 + i*h2) as u128 * num_bits as u128) >> 64`, which lowers to a
/// single `mulq` on x86_64.
///
/// The result is always in `[0, num_bits)` for any `num_bits`, not just
/// powers of two: the 128-bit product of a 64-bit value with `num_bits` is
/// at most `(2^64 - 1) * num_bits`, so its high 64 bits are at most
/// `num_bits - 1`. Unlike `%`, this is unbiased only to within one part in
/// 2^64, which is far below the noise floor of the filter itself.
#[inline(always)]
pub fn nth_index(num_bits: usize, h1: u64, h2: u64, i: u32) -> usize {
    let combined = h1.wrapping_add((i as u64).wrapping_mul(h2));
    ((combined as u128).wrapping_mul(num_bits as u128) >> 64) as usize
}

/// Iterator over the `k` probe indices for an item.
#[inline]
pub fn indices(num_bits: usize, hash: u64, k: u32) -> impl Iterator<Item = usize> {
    let (h1, h2) = double_hash(hash);
    (0..k).map(move |i| nth_index(num_bits, h1, h2, i))
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::collections::BTreeSet;

    #[test]
    fn no_fixed_point_at_zero() {
        // The fastbloom bug: a derivation with `h2` derived from `h1 * C`
        // had all-zero indices for hash==0. Verify our derivation produces
        // distinct probes.
        let probes: BTreeSet<_> = indices(1024, 0, 7).collect();
        assert!(
            probes.len() >= 6,
            "hash 0 collapsed to {} distinct probes",
            probes.len()
        );
    }

    #[test]
    fn no_fixed_point_at_max() {
        let probes: BTreeSet<_> = indices(1024, u64::MAX, 7).collect();
        assert!(probes.len() >= 6);
    }

    #[test]
    fn indices_in_range() {
        for hash in [0u64, 1, 0x12345678, u64::MAX] {
            for &m in &[64usize, 128, 1024, 1 << 16, 1 << 20] {
                for ix in indices(m, hash, 16) {
                    assert!(ix < m, "ix {ix} out of range for m {m}");
                }
            }
        }
    }
}
