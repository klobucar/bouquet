//! # bouquet — pluggable Bloom filters
//!
//! Pick your blooms. The storage layout and concurrency model of a Bloom
//! filter are wired in as a generic [`Backend`] type parameter, dispatched
//! statically (no `dyn`). Backends live behind cargo features so you only
//! compile what you use.
//!
//! ## Quickstart
//!
//! ```
//! # #[cfg(all(feature = "std", feature = "classical"))] {
//! use bouquet::{Filter, Classical};
//!
//! let filter = Filter::<Classical>::with_false_pos(0.001).expected_items(10_000);
//! filter.insert(&"hello");
//! assert!(filter.contains(&"hello"));
//! # }
//! ```
//!
//! See the crate `README.md` for the design pitch and feature matrix.

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

extern crate alloc;

pub mod backend;
mod hasher;
pub mod math;

#[cfg(feature = "serde")]
mod serde_impl;

#[cfg(all(feature = "serde", feature = "loom"))]
compile_error!("`serde` and `loom` are mutually exclusive features");

use core::fmt;
use core::hash::{BuildHasher, Hash};
use core::marker::PhantomData;

#[cfg(feature = "atomic")]
pub use backend::Atomic;
pub use backend::Backend;
#[cfg(feature = "blocked")]
pub use backend::Blocked;
#[cfg(feature = "classical")]
pub use backend::Classical;
#[cfg(feature = "simd")]
pub use backend::SimdBlocked;
pub use hasher::DefaultHasher;

use math::{MAX_HASHES, MIN_BITS};

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

/// A Bloom filter parameterised by its storage [`Backend`] and a
/// [`BuildHasher`].
///
/// Construct one through the builder entry points
/// [`Filter::with_false_pos`] or [`Filter::with_num_bits`]:
///
/// ```
/// # #[cfg(all(feature = "std", feature = "classical"))] {
/// use bouquet::{Filter, Classical};
/// let filter = Filter::<Classical>::with_false_pos(0.001).expected_items(10_000);
/// filter.insert(&"hello");
/// assert!(filter.contains(&"hello"));
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Filter<B: Backend, S: BuildHasher = DefaultHasher> {
    backend: B,
    hasher: S,
    num_hashes: u32,
}

#[cfg(feature = "std")]
impl<B: Backend> Filter<B, DefaultHasher> {
    /// Begin building a filter for a target false-positive rate `fp`. Call
    /// [`WithFalsePos::expected_items`] to finish.
    ///
    /// Panics if `fp` is not in the open interval `(0, 1)` or is NaN.
    pub fn with_false_pos(fp: f64) -> WithFalsePos<B, DefaultHasher> {
        if !fp.is_finite() || fp <= 0.0 || fp >= 1.0 {
            panic!("with_false_pos: fp must be in (0, 1), got {fp}");
        }
        WithFalsePos {
            fp,
            hasher: DefaultHasher::new(),
            _backend: PhantomData,
        }
    }

    /// Begin building a filter with an explicit `num_bits` capacity. Call
    /// [`WithNumBits::expected_items`] (to derive `k`) or
    /// [`WithNumBits::hashes`] (to set `k` directly) to finish.
    ///
    /// Panics if `num_bits == 0`.
    pub fn with_num_bits(num_bits: usize) -> WithNumBits<B, DefaultHasher> {
        if num_bits == 0 {
            panic!("with_num_bits: num_bits must be > 0");
        }
        WithNumBits {
            num_bits: round_up_bits(num_bits),
            hasher: DefaultHasher::new(),
            _backend: PhantomData,
        }
    }
}

#[cfg(not(feature = "std"))]
impl<B: Backend> Filter<B, DefaultHasher> {
    /// Begin building a filter for a target false-positive rate `fp` with a
    /// caller-provided seed. (Without `std` there is no source of randomness
    /// for a default seed.)
    pub fn with_false_pos_seeded(fp: f64, seed: u128) -> WithFalsePos<B, DefaultHasher> {
        if !fp.is_finite() || fp <= 0.0 || fp >= 1.0 {
            panic!("with_false_pos_seeded: fp must be in (0, 1)");
        }
        WithFalsePos {
            fp,
            hasher: DefaultHasher::with_seed(seed),
            _backend: PhantomData,
        }
    }

    /// Begin building a filter with explicit `num_bits` and a caller-provided
    /// seed.
    pub fn with_num_bits_seeded(num_bits: usize, seed: u128) -> WithNumBits<B, DefaultHasher> {
        if num_bits == 0 {
            panic!("with_num_bits_seeded: num_bits must be > 0");
        }
        WithNumBits {
            num_bits: round_up_bits(num_bits),
            hasher: DefaultHasher::with_seed(seed),
            _backend: PhantomData,
        }
    }
}

impl<B: Backend, S: BuildHasher> Filter<B, S> {
    /// Bit capacity of the filter. Always a multiple of 64.
    pub fn num_bits(&self) -> usize {
        self.backend.num_bits()
    }

    /// Number of hashes computed per item.
    pub fn num_hashes(&self) -> u32 {
        self.num_hashes
    }

    /// Reference to the configured `BuildHasher`.
    pub fn hasher(&self) -> &S {
        &self.hasher
    }

    /// Insert `item`. Returns `true` if every probe already saw a set bit
    /// before this call (i.e. the item was probably already present).
    ///
    /// Whether multiple threads can call `insert` concurrently depends on
    /// the backend: `Atomic` is `Sync`, `Classical` is not.
    #[inline]
    pub fn insert<T: Hash + ?Sized>(&self, item: &T) -> bool {
        let h = self.hash_of(item);
        self.backend.set_item(h, self.num_hashes)
    }

    /// Returns true if `item` is *probably* present. False positives are
    /// possible at the rate this filter was sized for; false negatives are
    /// not.
    #[inline]
    pub fn contains<T: Hash + ?Sized>(&self, item: &T) -> bool {
        let h = self.hash_of(item);
        self.backend.check_item(h, self.num_hashes)
    }

    /// Reset every bit to zero. Takes `&mut self`, so concurrent inserts
    /// are statically prevented from racing with `clear`.
    pub fn clear(&mut self) {
        self.backend.clear();
    }

    /// Estimate the number of distinct items inserted, from the popcount of
    /// the underlying bit vector. Accuracy degrades as the filter saturates.
    pub fn estimate_count(&self) -> usize {
        let m = self.backend.num_bits() as f64;
        let k = self.num_hashes as f64;
        let x = self.backend.count_ones() as f64;
        if x >= m {
            return usize::MAX;
        }
        let est = -(m / k) * libm::log(1.0 - x / m);
        if est.is_finite() && est >= 0.0 {
            libm::round(est) as usize
        } else {
            0
        }
    }

    #[inline]
    fn hash_of<T: Hash + ?Sized>(&self, item: &T) -> u64 {
        self.hasher.hash_one(item)
    }
}

impl<B: Backend, S: BuildHasher + PartialEq> Filter<B, S> {
    /// Returns whether two filters can safely be combined. Filters with
    /// different hashers, sizes, or `k` values are *incompatible*: combining
    /// them produces nonsense.
    pub fn is_compatible(&self, other: &Self) -> bool {
        self.num_hashes == other.num_hashes
            && self.backend.num_bits() == other.backend.num_bits()
            && self.hasher == other.hasher
    }
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Builder returned by [`Filter::with_false_pos`].
#[derive(Clone, Debug)]
pub struct WithFalsePos<B: Backend, S: BuildHasher> {
    fp: f64,
    hasher: S,
    _backend: PhantomData<fn() -> B>,
}

impl<B: Backend, S: BuildHasher> WithFalsePos<B, S> {
    /// Use a different `BuildHasher`. The hasher chosen here is what the
    /// resulting filter will hold for the rest of its life — pick carefully,
    /// because two filters with different hashers cannot be compared,
    /// unioned, or intersected.
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> WithFalsePos<B, S2> {
        WithFalsePos {
            fp: self.fp,
            hasher,
            _backend: PhantomData,
        }
    }

    /// Finish the build. Sizes the filter for `expected_items` items at the
    /// false-positive rate set on the builder.
    ///
    /// Panics if `expected_items == 0`.
    pub fn expected_items(self, expected_items: usize) -> Filter<B, S> {
        let num_bits = math::optimal_num_bits(expected_items, self.fp)
            .expect("expected_items must be > 0 and fp must be in (0, 1)");
        let num_hashes = math::optimal_num_hashes(num_bits, expected_items)
            .expect("optimal_num_hashes failed despite valid inputs");
        Filter::from_parts(num_bits, num_hashes, self.hasher)
    }

    /// Alias for [`Self::expected_items`].
    pub fn items(self, items: usize) -> Filter<B, S> {
        self.expected_items(items)
    }
}

impl<B: Backend> WithFalsePos<B, DefaultHasher> {
    /// Use an explicit 128-bit seed for the default hasher. Two filters
    /// built with the same seed will hash identically.
    pub fn seed(self, seed: u128) -> Self {
        Self {
            fp: self.fp,
            hasher: DefaultHasher::with_seed(seed),
            _backend: PhantomData,
        }
    }
}

/// Builder returned by [`Filter::with_num_bits`].
#[derive(Clone, Debug)]
pub struct WithNumBits<B: Backend, S: BuildHasher> {
    num_bits: usize,
    hasher: S,
    _backend: PhantomData<fn() -> B>,
}

impl<B: Backend, S: BuildHasher> WithNumBits<B, S> {
    /// Swap the `BuildHasher`.
    pub fn hasher<S2: BuildHasher>(self, hasher: S2) -> WithNumBits<B, S2> {
        WithNumBits {
            num_bits: self.num_bits,
            hasher,
            _backend: PhantomData,
        }
    }

    /// Finish the build, deriving `k` from `expected_items`.
    ///
    /// Panics if `expected_items == 0`.
    pub fn expected_items(self, expected_items: usize) -> Filter<B, S> {
        let num_hashes = math::optimal_num_hashes(self.num_bits, expected_items)
            .expect("expected_items must be > 0");
        Filter::from_parts(self.num_bits, num_hashes, self.hasher)
    }

    /// Alias for [`Self::expected_items`].
    pub fn items(self, items: usize) -> Filter<B, S> {
        self.expected_items(items)
    }

    /// Finish the build with an explicit number of hash functions.
    ///
    /// Panics if `k == 0` or `k > MAX_HASHES`.
    pub fn hashes(self, k: u32) -> Filter<B, S> {
        if k == 0 || k > MAX_HASHES {
            panic!("hashes: k must be in 1..={MAX_HASHES}, got {k}");
        }
        Filter::from_parts(self.num_bits, k, self.hasher)
    }
}

impl<B: Backend> WithNumBits<B, DefaultHasher> {
    /// Use an explicit 128-bit seed for the default hasher.
    pub fn seed(self, seed: u128) -> Self {
        Self {
            num_bits: self.num_bits,
            hasher: DefaultHasher::with_seed(seed),
            _backend: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

impl<B: Backend, S: BuildHasher> Filter<B, S> {
    pub(crate) fn from_parts(num_bits: usize, num_hashes: u32, hasher: S) -> Self {
        debug_assert!(num_bits >= MIN_BITS && num_bits.is_multiple_of(64));
        debug_assert!((1..=MAX_HASHES).contains(&num_hashes));
        Self {
            backend: B::with_capacity(num_bits),
            hasher,
            num_hashes,
        }
    }

    /// Reconstruct from raw parts. Used by the `serde` deserializer after it
    /// has validated every input.
    #[cfg(feature = "serde")]
    pub(crate) fn from_validated_parts(backend: B, num_hashes: u32, hasher: S) -> Self {
        Self {
            backend,
            hasher,
            num_hashes,
        }
    }
}

fn round_up_bits(num_bits: usize) -> usize {
    let snapped = num_bits.div_ceil(64) * 64;
    snapped.max(MIN_BITS)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned from validated entry points (currently only the `serde`
/// deserializer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Filter has zero bits.
    EmptyBits,
    /// `num_hashes` is zero or above [`math::MAX_HASHES`].
    InvalidHashes(u32),
    /// `num_bits` does not match `bits.len() * 64`.
    InconsistentNumBits {
        /// Bit count claimed in the serialized form.
        claimed: usize,
        /// Bit count derived from the word array length.
        derived: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::EmptyBits => f.write_str("Bloom filter has zero bits"),
            Error::InvalidHashes(k) => write!(
                f,
                "num_hashes {k} is out of range (must be 1..={})",
                MAX_HASHES
            ),
            Error::InconsistentNumBits { claimed, derived } => write!(
                f,
                "num_bits ({claimed}) does not match bits.len() * 64 ({derived})",
            ),
        }
    }
}

impl core::error::Error for Error {}

// A handful of Filter-level tests that work without `std`, exercising the
// `_seeded` entry points. Keeps `cargo test --no-default-features --features
// classical` from being a no-op.
#[cfg(all(test, not(feature = "std"), feature = "classical"))]
mod no_std_tests {
    use super::*;

    extern crate alloc;
    use alloc::format;

    #[test]
    fn seeded_round_trip() {
        let f = Filter::<Classical>::with_false_pos_seeded(0.01, 0xdead_beef).expected_items(1_000);
        for i in 0..200u32 {
            let s = format!("item-{i}");
            f.insert(&s);
        }
        for i in 0..200u32 {
            let s = format!("item-{i}");
            assert!(f.contains(&s));
        }
    }

    #[test]
    fn seeded_with_num_bits_and_explicit_hashes() {
        let f = Filter::<Classical>::with_num_bits_seeded(1024, 0).hashes(4);
        assert_eq!(f.num_bits(), 1024);
        assert_eq!(f.num_hashes(), 4);
    }
}

// High-level Filter tests use the `std`-only `with_false_pos` / `with_num_bits`
// entry points. Backend-internal correctness is exercised separately in each
// backend's `mod tests` (which run under `--no-default-features --features
// classical`).
#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    extern crate alloc;
    use alloc::string::String;
    use alloc::vec::Vec;

    #[cfg(feature = "classical")]
    #[test]
    fn round_trip_strings() {
        let f = Filter::<Classical>::with_false_pos(0.01).expected_items(10_000);
        for i in 0..1000u32 {
            let s = alloc::format!("item-{i}");
            f.insert(&s);
        }
        for i in 0..1000u32 {
            let s = alloc::format!("item-{i}");
            assert!(f.contains(&s), "missing item {i}");
        }
    }

    #[cfg(feature = "classical")]
    #[test]
    fn explicit_num_bits_and_hashes() {
        let f = Filter::<Classical>::with_num_bits(1024).hashes(4);
        assert_eq!(f.num_bits(), 1024);
        assert_eq!(f.num_hashes(), 4);
    }

    #[cfg(feature = "classical")]
    #[test]
    fn explicit_seed_round_trips() {
        let a = Filter::<Classical>::with_false_pos(0.01)
            .seed(0xdead_beef)
            .expected_items(1000);
        let b = Filter::<Classical>::with_false_pos(0.01)
            .seed(0xdead_beef)
            .expected_items(1000);
        for i in 0..100u32 {
            a.insert(&i);
            b.insert(&i);
        }
        assert_eq!(a.estimate_count(), b.estimate_count());
        assert!(a.is_compatible(&b));
    }

    #[cfg(feature = "classical")]
    #[test]
    fn estimate_count_close_to_truth() {
        let f = Filter::<Classical>::with_false_pos(0.001).expected_items(10_000);
        let mut inserted: Vec<String> = Vec::new();
        for i in 0..1000u32 {
            let s = alloc::format!("item-{i}");
            f.insert(&s);
            inserted.push(s);
        }
        let est = f.estimate_count();
        assert!(
            (900..=1100).contains(&est),
            "estimate {est} too far from 1000"
        );
    }

    #[cfg(feature = "classical")]
    #[test]
    #[should_panic(expected = "fp must be in (0, 1)")]
    fn rejects_zero_fp() {
        let _ = Filter::<Classical>::with_false_pos(0.0);
    }

    #[cfg(feature = "classical")]
    #[test]
    #[should_panic(expected = "fp must be in (0, 1)")]
    fn rejects_one_fp() {
        let _ = Filter::<Classical>::with_false_pos(1.0);
    }

    #[cfg(feature = "classical")]
    #[test]
    #[should_panic(expected = "num_bits must be > 0")]
    fn rejects_zero_bits() {
        let _ = Filter::<Classical>::with_num_bits(0);
    }

    #[cfg(feature = "classical")]
    #[test]
    #[should_panic(expected = "k must be in")]
    fn rejects_zero_hashes() {
        let _ = Filter::<Classical>::with_num_bits(1024).hashes(0);
    }

    #[cfg(feature = "classical")]
    #[test]
    #[should_panic(expected = "k must be in")]
    fn rejects_too_many_hashes() {
        let _ = Filter::<Classical>::with_num_bits(1024).hashes(MAX_HASHES + 1);
    }

    /// Sanity check that the atomic-backed filter actually accepts concurrent
    /// `&Filter::insert` calls and produces a coherent result. This is *not*
    /// a model-checking test; the loom-based one in `tests/loom_atomic.rs`
    /// is.
    #[cfg(feature = "atomic")]
    #[test]
    fn atomic_filter_concurrent_insert() {
        use std::sync::Arc;
        use std::thread;

        let f = Arc::new(
            Filter::<Atomic>::with_false_pos(0.01)
                .seed(0xfeed_face)
                .expected_items(40_000),
        );

        let mut handles = Vec::new();
        for t in 0..4u32 {
            let f = Arc::clone(&f);
            handles.push(thread::spawn(move || {
                for i in 0..10_000u32 {
                    f.insert(&(t * 1_000_000 + i));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        for t in 0..4u32 {
            for i in 0..10_000u32 {
                assert!(f.contains(&(t * 1_000_000 + i)));
            }
        }
    }
}
