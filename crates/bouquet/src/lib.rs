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
//! use bouquet::ClassicalFilter;
//!
//! let filter = ClassicalFilter::with_false_pos(0.001).expected_items(10_000);
//! filter.insert(&"hello");
//! assert!(filter.contains(&"hello"));
//! # }
//! ```
//!
//! See the crate `README.md` for the design pitch and feature matrix.

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
// `unsafe fn` bodies do not get an implicit unsafe block. This is the
// edition-2024 behaviour; opting in early keeps every unsafe operation
// paired with the SAFETY comment that justifies it.
#![deny(unsafe_op_in_unsafe_fn)]

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
/// Most callers don't write `Filter<Backend>` directly — they reach for one
/// of the per-backend type aliases [`ClassicalFilter`], [`AtomicFilter`],
/// [`BlockedFilter`], or [`SimdBlockedFilter`], which spell the backend in
/// the name and skip the turbofish:
///
/// ```
/// # #[cfg(all(feature = "std", feature = "classical"))] {
/// use bouquet::ClassicalFilter;
/// let filter = ClassicalFilter::with_false_pos(0.001).expected_items(10_000);
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

/// Single-threaded filter with the flat textbook layout. Alias for
/// `Filter<Classical, S>`.
#[cfg(feature = "classical")]
pub type ClassicalFilter<S = DefaultHasher> = Filter<Classical, S>;

/// Concurrent filter, `Sync`, `&self` insert. Alias for `Filter<Atomic, S>`.
#[cfg(feature = "atomic")]
pub type AtomicFilter<S = DefaultHasher> = Filter<Atomic, S>;

/// Single-threaded filter with cache-line-sized blocks. Alias for
/// `Filter<Blocked, S>`.
#[cfg(feature = "blocked")]
pub type BlockedFilter<S = DefaultHasher> = Filter<Blocked, S>;

/// `Blocked` with NEON / AVX2 hot loop. Alias for `Filter<SimdBlocked, S>`.
#[cfg(feature = "simd")]
pub type SimdBlockedFilter<S = DefaultHasher> = Filter<SimdBlocked, S>;

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

    /// Returns whether every bit in the filter is set.
    ///
    /// A saturated filter answers `true` to every [`Self::contains`] query
    /// and its [`Self::estimate_count`] is meaningless. Worth checking
    /// before trusting an estimate from a filter you did not size yourself
    /// — for instance one that arrived over the wire.
    pub fn is_saturated(&self) -> bool {
        self.backend.count_ones() >= self.backend.num_bits()
    }

    /// Estimate the number of distinct items inserted, from the popcount of
    /// the underlying bit vector. Accuracy degrades as the filter saturates.
    ///
    /// Returns [`usize::MAX`] as a sentinel when the filter is fully
    /// saturated — the estimator's `ln(1 - x/m)` term diverges there, so
    /// there is no finite answer to give. Check [`Self::is_saturated`]
    /// first if you intend to do arithmetic on the result; in particular,
    /// feeding it straight to `Vec::with_capacity` will abort the process
    /// on a saturated filter.
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
    /// Panics if `expected_items == 0`, or if the computed size overflows
    /// `usize` (only reachable with astronomically large `expected_items`).
    pub fn expected_items(self, expected_items: usize) -> Filter<B, S> {
        let num_bits = math::optimal_num_bits(expected_items, self.fp)
            .expect("expected_items must be > 0 and the computed filter size must fit in usize");
        let num_hashes = math::optimal_num_hashes(num_bits, expected_items)
            .expect("optimal_num_hashes failed despite valid inputs");
        Filter::from_parts(num_bits, num_hashes, self.hasher)
    }

    /// Alias for [`Self::expected_items`].
    pub fn items(self, items: usize) -> Filter<B, S> {
        self.expected_items(items)
    }

    /// Fallible [`Self::expected_items`].
    ///
    /// Prefer this whenever `expected_items` comes from outside the program
    /// — a request parameter, a config file, a language binding. The
    /// infallible form panics on a size that overflows and *aborts the
    /// process* on a size the allocator cannot satisfy.
    pub fn try_expected_items(self, expected_items: usize) -> Result<Filter<B, S>, Error> {
        let num_bits =
            math::optimal_num_bits(expected_items, self.fp).ok_or(Error::CapacityOverflow {
                requested_bits: expected_items,
            })?;
        let num_hashes =
            math::optimal_num_hashes(num_bits, expected_items).ok_or(Error::CapacityOverflow {
                requested_bits: num_bits,
            })?;
        Filter::try_from_parts(num_bits, num_hashes, self.hasher)
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
    /// Note that blocked backends clamp `k` to
    /// [`backend::blocked::MAX_BLOCK_HASHES`]; [`Filter::num_hashes`]
    /// reports the clamped value.
    ///
    /// Panics if `k == 0` or `k > MAX_HASHES`.
    pub fn hashes(self, k: u32) -> Filter<B, S> {
        if k == 0 || k > MAX_HASHES {
            panic!("hashes: k must be in 1..={MAX_HASHES}, got {k}");
        }
        Filter::from_parts(self.num_bits, k, self.hasher)
    }

    /// Fallible [`Self::expected_items`]. See
    /// [`WithFalsePos::try_expected_items`] for when to prefer it.
    pub fn try_expected_items(self, expected_items: usize) -> Result<Filter<B, S>, Error> {
        let num_hashes = math::optimal_num_hashes(self.num_bits, expected_items).ok_or(
            Error::CapacityOverflow {
                requested_bits: self.num_bits,
            },
        )?;
        Filter::try_from_parts(self.num_bits, num_hashes, self.hasher)
    }

    /// Fallible [`Self::hashes`]: returns [`Error::InvalidHashes`] instead
    /// of panicking on an out-of-range `k`, and reports allocation failure
    /// instead of aborting.
    pub fn try_hashes(self, k: u32) -> Result<Filter<B, S>, Error> {
        if k == 0 || k > MAX_HASHES {
            return Err(Error::InvalidHashes(k));
        }
        Filter::try_from_parts(self.num_bits, k, self.hasher)
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
        // Clamp once, here, so `num_hashes()` reports the `k` the probe
        // loops will actually run. Blocked backends cap this well below
        // `MAX_HASHES`; see `backend::Backend::clamp_hashes`.
        let num_hashes = B::clamp_hashes(num_hashes);
        Self {
            backend: B::with_capacity(num_bits),
            hasher,
            num_hashes,
        }
    }

    /// Fallible [`Self::from_parts`]. Propagates capacity overflow and
    /// allocation failure instead of panicking / aborting.
    pub(crate) fn try_from_parts(
        num_bits: usize,
        num_hashes: u32,
        hasher: S,
    ) -> Result<Self, Error> {
        let num_hashes = B::clamp_hashes(num_hashes);
        Ok(Self {
            backend: B::try_with_capacity(num_bits)?,
            hasher,
            num_hashes,
        })
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
    let snapped = num_bits
        .div_ceil(64)
        .checked_mul(64)
        .expect("with_num_bits: num_bits too large to round up to a word");
    snapped.max(MIN_BITS)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned from validated entry points (currently only the `serde`
/// deserializer).
///
/// Marked `#[non_exhaustive]`: new validation rules add variants, and that
/// should not be a breaking change for downstream `match`es.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
    /// `bits` length is not valid for the backend's layout (e.g. a blocked
    /// backend requires a whole number of 512-bit blocks).
    InvalidWordCount(usize),
    /// The requested size cannot be rounded up to the backend's layout
    /// granularity without overflowing `usize`.
    CapacityOverflow {
        /// Bit count that was requested.
        requested_bits: usize,
    },
    /// The allocator could not provide storage for a filter this large.
    ///
    /// Returned by the `try_*` constructors. The infallible constructors
    /// abort the process instead, because Rust's global allocator calls
    /// `handle_alloc_error` on failure — use the `try_*` forms whenever the
    /// size is derived from untrusted input.
    AllocationFailed {
        /// Bit count whose allocation failed.
        bits: usize,
    },
    /// The serialized filter was written by a backend with a different bit
    /// layout or probe scheme — for example a `Filter<Classical>` blob being
    /// loaded as a `Filter<Blocked>`. Loading it anyway would produce false
    /// negatives, so it is rejected. See [`backend::Layout`].
    LayoutMismatch {
        /// Layout tag the target backend expects.
        expected: &'static str,
        /// Layout tag found in the serialized form.
        found: alloc::string::String,
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
            Error::InvalidWordCount(words) => write!(
                f,
                "bits length ({words} words) is not valid for this backend's layout",
            ),
            Error::CapacityOverflow { requested_bits } => write!(
                f,
                "requested filter size ({requested_bits} bits) overflows usize when rounded \
                 up to the backend's layout granularity",
            ),
            Error::AllocationFailed { bits } => {
                write!(f, "could not allocate storage for a {bits}-bit filter",)
            }
            Error::LayoutMismatch { expected, found } => write!(
                f,
                "filter was serialized with layout '{found}' but this backend uses '{expected}'; \
                 loading it would produce false negatives",
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
    ///
    /// Not built under `loom`: there `Atomic` uses loom's atomics, which
    /// panic when touched from plain `std` threads outside a `loom::model`.
    #[cfg(all(feature = "atomic", not(feature = "loom")))]
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
