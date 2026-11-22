//! SIMD-accelerated blocked Bloom backend.
//!
//! Same indexing scheme as [`super::blocked::Blocked`] (512-bit
//! cache-line-sized blocks, in-block double hashing), but the hot loop
//! builds a single 512-bit "needs-set" mask scalar and then does the
//! load–test–store as wide vector ops:
//!
//! - **aarch64**: 4× 128-bit NEON registers per block. NEON is mandatory in
//!   the ARMv8 spec, so it is selected unconditionally on this target.
//! - **x86_64 with `target-feature=+avx2`** (or `target-cpu=native` on any
//!   recent x86 chip): 2× 256-bit AVX2 registers per block.
//! - **everywhere else**: forwards to the scalar [`super::blocked::Blocked`]
//!   path.
//!
//! The x86_64 path uses *compile-time* feature detection
//! (`cfg(target_feature = "avx2")`) rather than runtime detection — that
//! keeps `simd` usable from `no_std` and avoids the cost of a per-call
//! `cpuid` check. Users who want the AVX2 path should compile with
//! `RUSTFLAGS="-C target-cpu=native"` or an explicit
//! `RUSTFLAGS="-C target-feature=+avx2"`.
//!
//! AVX-512 is intentionally not implemented yet — the frequency
//! throttling story on Intel client CPUs is too messy for a library to
//! enable by default. A future revision can add it behind its own feature.
//!
//! ### Why mask-build-then-store, instead of "build mask in vectors"
//!
//! With `k ≤ 7` and 8-lane blocks, building the OR-mask probe-by-probe in
//! vector registers requires per-lane shifts that the SIMD ISAs of interest
//! don't have a clean form for. Building the mask scalar (a `[u64; 8]`) and
//! then doing one wide load + AND + compare + OR + store is simpler, and
//! the actual *cost* is dominated by the cache line round trip, not the
//! mask construction.

use alloc::{boxed::Box, vec};
use core::cell::Cell;

use super::blocked::{block_index, effective_k, BlockProbes, BLOCK_BITS};
use super::{Backend, Layout};

const BLOCK_WORDS: usize = BLOCK_BITS / 64;

/// SIMD blocked Bloom backend. Same memory layout as
/// [`super::blocked::Blocked`]; faster hot loop on x86_64 (with AVX2) and
/// aarch64.
#[derive(Debug)]
pub struct SimdBlocked {
    bits: Box<[Cell<u64>]>,
    num_bits: usize,
    num_blocks: usize,
}

impl Clone for SimdBlocked {
    fn clone(&self) -> Self {
        let mut copy = vec![Cell::new(0u64); self.bits.len()].into_boxed_slice();
        for (dst, src) in copy.iter_mut().zip(self.bits.iter()) {
            dst.set(src.get());
        }
        Self {
            bits: copy,
            num_bits: self.num_bits,
            num_blocks: self.num_blocks,
        }
    }
}

/// Union of the `k` probe bits for `hash`, as a 512-bit block-shaped mask.
///
/// Uses the same `BlockProbes` stream and the same `effective_k` clamp as
/// the scalar [`super::blocked::Blocked`] loops, which is what keeps the two
/// backends bit-for-bit identical.
#[inline(always)]
fn build_mask(hash: u64, k: u32) -> [u64; BLOCK_WORDS] {
    let mut probes = BlockProbes::new(hash);
    let k = effective_k(k);
    let mut mask = [0u64; BLOCK_WORDS];
    for _ in 0..k {
        let in_ix = probes.next_probe() as usize;
        let word = in_ix >> 6;
        let bit = 1u64 << (in_ix & 63);
        // Select the word with a compare instead of indexing by it. A
        // data-dependent `mask[word] |= bit` forces the array onto the
        // stack as a chain of 8-byte read-modify-writes, and the 16/32-byte
        // vector loads that follow cannot be store-forwarded from those —
        // they stall until the stores retire, which serialises consecutive
        // operations. With constant indices the mask lives in registers.
        for (j, m) in mask.iter_mut().enumerate() {
            *m |= if j == word { bit } else { 0 };
        }
    }
    mask
}

impl Backend for SimdBlocked {
    const LAYOUT: Layout = Layout::Blocked512;

    fn clamp_hashes(k: u32) -> u32 {
        effective_k(k)
    }

    fn with_capacity(num_bits: usize) -> Self {
        Self::try_with_capacity(num_bits).expect("SimdBlocked::with_capacity")
    }

    fn try_with_capacity(num_bits: usize) -> Result<Self, crate::Error> {
        // See `Blocked::try_with_capacity`: round up unconditionally so a
        // direct call with an arbitrary size cannot break the
        // `get_unchecked` invariant in the hot loops.
        let snapped = super::round_up_to(num_bits, BLOCK_BITS)?;
        let num_blocks = snapped / BLOCK_BITS;
        let words = snapped / 64;
        let bits = super::try_alloc_words(words, snapped, || Cell::new(0u64))?;
        Ok(Self {
            bits: bits.into_boxed_slice(),
            num_bits: snapped,
            num_blocks,
        })
    }

    #[inline]
    fn num_bits(&self) -> usize {
        self.num_bits
    }

    #[inline]
    fn set_item(&self, hash: u64, k: u32) -> bool {
        let block = block_index(hash, self.num_blocks);
        let word_offset = block * BLOCK_WORDS;
        let mask = build_mask(hash, k);
        // SAFETY: Cell<u64> is repr(transparent) over u64, so the slice
        // is layout-compatible with [u64]. Interior mutability through
        // Cell makes it sound to mutate via raw pointer with &self.
        let block_ptr = unsafe { (self.bits.as_ptr() as *mut u64).add(word_offset) };
        debug_assert!(word_offset + BLOCK_WORDS <= self.bits.len());

        unsafe { dispatch_set(block_ptr, &mask) }
    }

    #[inline]
    fn check_item(&self, hash: u64, k: u32) -> bool {
        let block = block_index(hash, self.num_blocks);
        let word_offset = block * BLOCK_WORDS;
        let mask = build_mask(hash, k);
        let block_ptr = unsafe { (self.bits.as_ptr() as *const u64).add(word_offset) };
        debug_assert!(word_offset + BLOCK_WORDS <= self.bits.len());

        unsafe { dispatch_check(block_ptr, &mask) }
    }

    fn count_ones(&self) -> usize {
        self.bits
            .iter()
            .map(|c| c.get().count_ones() as usize)
            .sum()
    }

    fn clear(&mut self) {
        for c in self.bits.iter() {
            c.set(0);
        }
    }

    fn union_with(&mut self, other: &Self) {
        assert_eq!(self.num_bits, other.num_bits, "union_with: size mismatch");
        for (dst, src) in self.bits.iter_mut().zip(other.bits.iter()) {
            *dst.get_mut() |= src.get();
        }
    }

    fn intersect_with(&mut self, other: &Self) {
        assert_eq!(
            self.num_bits, other.num_bits,
            "intersect_with: size mismatch"
        );
        for (dst, src) in self.bits.iter_mut().zip(other.bits.iter()) {
            *dst.get_mut() &= src.get();
        }
    }

    #[cfg(feature = "serde")]
    fn snapshot_words(&self) -> alloc::vec::Vec<u64> {
        self.bits.iter().map(|c| c.get()).collect()
    }

    #[cfg(feature = "serde")]
    fn from_words(words: alloc::vec::Vec<u64>) -> Option<Self> {
        if words.is_empty() || !words.len().is_multiple_of(BLOCK_WORDS) {
            return None;
        }
        let num_bits = words.len() * 64;
        let num_blocks = num_bits / BLOCK_BITS;
        let bits: alloc::vec::Vec<Cell<u64>> = words.into_iter().map(Cell::new).collect();
        Some(Self {
            bits: bits.into_boxed_slice(),
            num_bits,
            num_blocks,
        })
    }
}

// ---------------------------------------------------------------------------
// Architecture dispatch
// ---------------------------------------------------------------------------

/// Selectors for the in-block hot path. One `dispatch_set` and one
/// `dispatch_check` are compiled per target; an unused arch falls through
/// to the scalar path.
///
/// # Safety
///
/// Every function in this section, and every arch backend it forwards to,
/// shares one precondition: `block_ptr` must be valid for reads (and, for
/// the `set` family, writes) of [`BLOCK_WORDS`] consecutive `u64` values,
/// and properly aligned for `u64`.
///
/// `SimdBlocked` discharges this by construction: `with_capacity` rounds up
/// to a whole number of blocks and floors at one, `from_words` rejects any
/// word count that is not a multiple of `BLOCK_WORDS`, and the callers
/// derive `word_offset` as `block * BLOCK_WORDS` with
/// `block < num_blocks == bits.len() / BLOCK_WORDS`.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn dispatch_set(block_ptr: *mut u64, mask: &[u64; BLOCK_WORDS]) -> bool {
    // SAFETY: precondition forwarded to the caller of `dispatch_set`.
    unsafe { neon::set(block_ptr, mask) }
}

/// # Safety
///
/// See [`dispatch_set`].
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline]
unsafe fn dispatch_set(block_ptr: *mut u64, mask: &[u64; BLOCK_WORDS]) -> bool {
    // SAFETY: precondition forwarded to the caller of `dispatch_set`.
    unsafe { avx2::set(block_ptr, mask) }
}

/// # Safety
///
/// See [`dispatch_set`].
#[cfg(not(any(
    target_arch = "aarch64",
    all(target_arch = "x86_64", target_feature = "avx2"),
)))]
#[inline]
unsafe fn dispatch_set(block_ptr: *mut u64, mask: &[u64; BLOCK_WORDS]) -> bool {
    // SAFETY: precondition forwarded to the caller of `dispatch_set`.
    unsafe { scalar::set(block_ptr, mask) }
}

/// # Safety
///
/// See [`dispatch_set`]; `block_ptr` need only be valid for reads.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn dispatch_check(block_ptr: *const u64, mask: &[u64; BLOCK_WORDS]) -> bool {
    // SAFETY: precondition forwarded to the caller of `dispatch_check`.
    unsafe { neon::check(block_ptr, mask) }
}

/// # Safety
///
/// See [`dispatch_check`].
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline]
unsafe fn dispatch_check(block_ptr: *const u64, mask: &[u64; BLOCK_WORDS]) -> bool {
    // SAFETY: precondition forwarded to the caller of `dispatch_check`.
    unsafe { avx2::check(block_ptr, mask) }
}

/// # Safety
///
/// See [`dispatch_check`].
#[cfg(not(any(
    target_arch = "aarch64",
    all(target_arch = "x86_64", target_feature = "avx2"),
)))]
#[inline]
unsafe fn dispatch_check(block_ptr: *const u64, mask: &[u64; BLOCK_WORDS]) -> bool {
    // SAFETY: precondition forwarded to the caller of `dispatch_check`.
    unsafe { scalar::check(block_ptr, mask) }
}

// ---------------------------------------------------------------------------
// Scalar fallback
// ---------------------------------------------------------------------------

#[cfg(not(any(
    target_arch = "aarch64",
    all(target_arch = "x86_64", target_feature = "avx2"),
)))]
mod scalar {
    use super::BLOCK_WORDS;

    /// # Safety
    ///
    /// `block_ptr` must be valid for reads and writes of [`BLOCK_WORDS`]
    /// consecutive `u64`s and aligned for `u64`.
    #[inline]
    pub(super) unsafe fn set(block_ptr: *mut u64, mask: &[u64; BLOCK_WORDS]) -> bool {
        let mut all_set = true;
        for i in 0..BLOCK_WORDS {
            // SAFETY: `i < BLOCK_WORDS` and `mask` has exactly that length.
            let m = unsafe { *mask.get_unchecked(i) };
            if m == 0 {
                continue;
            }
            // SAFETY: `i < BLOCK_WORDS`, and the caller guarantees
            // `block_ptr` covers that many writable `u64`s.
            unsafe {
                let p = block_ptr.add(i);
                let cur = *p;
                if cur & m != m {
                    all_set = false;
                }
                *p = cur | m;
            }
        }
        all_set
    }

    /// # Safety
    ///
    /// `block_ptr` must be valid for reads of [`BLOCK_WORDS`] consecutive
    /// `u64`s and aligned for `u64`.
    #[inline]
    pub(super) unsafe fn check(block_ptr: *const u64, mask: &[u64; BLOCK_WORDS]) -> bool {
        for i in 0..BLOCK_WORDS {
            // SAFETY: `i < BLOCK_WORDS` and `mask` has exactly that length.
            let m = unsafe { *mask.get_unchecked(i) };
            if m == 0 {
                continue;
            }
            // SAFETY: `i < BLOCK_WORDS`, and the caller guarantees
            // `block_ptr` covers that many readable `u64`s.
            let cur = unsafe { *block_ptr.add(i) };
            if cur & m != m {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// aarch64 NEON
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::BLOCK_WORDS;
    use core::arch::aarch64::*;

    /// # Safety
    ///
    /// `block_ptr` must be valid for reads and writes of [`BLOCK_WORDS`]
    /// consecutive `u64`s and aligned for `u64`. NEON is mandatory in the
    /// ARMv8 spec, so no target-feature check is needed on this target.
    #[inline]
    pub(super) unsafe fn set(block_ptr: *mut u64, mask: &[u64; BLOCK_WORDS]) -> bool {
        // SAFETY: `mask` is a `[u64; 8]`, so lanes 0..8 are in bounds; the
        // caller guarantees `block_ptr` covers 8 readable+writable `u64`s.
        // The intrinsics are unaligned loads/stores, so `u64` alignment is
        // more than sufficient.
        unsafe {
            let mp = mask.as_ptr();
            let m0 = vld1q_u64(mp);
            let m1 = vld1q_u64(mp.add(2));
            let m2 = vld1q_u64(mp.add(4));
            let m3 = vld1q_u64(mp.add(6));

            let b0 = vld1q_u64(block_ptr);
            let b1 = vld1q_u64(block_ptr.add(2));
            let b2 = vld1q_u64(block_ptr.add(4));
            let b3 = vld1q_u64(block_ptr.add(6));

            // (b & m) == m  per lane → all-1s if every required bit was set.
            let eq0 = vceqq_u64(vandq_u64(b0, m0), m0);
            let eq1 = vceqq_u64(vandq_u64(b1, m1), m1);
            let eq2 = vceqq_u64(vandq_u64(b2, m2), m2);
            let eq3 = vceqq_u64(vandq_u64(b3, m3), m3);

            let combined = vandq_u64(vandq_u64(eq0, eq1), vandq_u64(eq2, eq3));
            let lo = vgetq_lane_u64(combined, 0);
            let hi = vgetq_lane_u64(combined, 1);
            let all_set = (lo & hi) == u64::MAX;

            // Always store the OR; if some bits weren't set we set them now.
            vst1q_u64(block_ptr, vorrq_u64(b0, m0));
            vst1q_u64(block_ptr.add(2), vorrq_u64(b1, m1));
            vst1q_u64(block_ptr.add(4), vorrq_u64(b2, m2));
            vst1q_u64(block_ptr.add(6), vorrq_u64(b3, m3));

            all_set
        }
    }

    /// # Safety
    ///
    /// `block_ptr` must be valid for reads of [`BLOCK_WORDS`] consecutive
    /// `u64`s and aligned for `u64`.
    #[inline]
    pub(super) unsafe fn check(block_ptr: *const u64, mask: &[u64; BLOCK_WORDS]) -> bool {
        // SAFETY: as in `set`, minus the stores.
        unsafe {
            let mp = mask.as_ptr();
            let m0 = vld1q_u64(mp);
            let m1 = vld1q_u64(mp.add(2));
            let m2 = vld1q_u64(mp.add(4));
            let m3 = vld1q_u64(mp.add(6));

            let b0 = vld1q_u64(block_ptr);
            let b1 = vld1q_u64(block_ptr.add(2));
            let b2 = vld1q_u64(block_ptr.add(4));
            let b3 = vld1q_u64(block_ptr.add(6));

            let eq0 = vceqq_u64(vandq_u64(b0, m0), m0);
            let eq1 = vceqq_u64(vandq_u64(b1, m1), m1);
            let eq2 = vceqq_u64(vandq_u64(b2, m2), m2);
            let eq3 = vceqq_u64(vandq_u64(b3, m3), m3);

            let combined = vandq_u64(vandq_u64(eq0, eq1), vandq_u64(eq2, eq3));
            let lo = vgetq_lane_u64(combined, 0);
            let hi = vgetq_lane_u64(combined, 1);
            (lo & hi) == u64::MAX
        }
    }
}

// ---------------------------------------------------------------------------
// x86_64 AVX2
// ---------------------------------------------------------------------------

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
mod avx2 {
    use super::BLOCK_WORDS;
    use core::arch::x86_64::*;

    /// # Safety
    ///
    /// `block_ptr` must be valid for reads and writes of [`BLOCK_WORDS`]
    /// consecutive `u64`s and aligned for `u64`. The module is gated on
    /// `target_feature = "avx2"`, so AVX2 availability is a compile-time
    /// guarantee rather than a caller obligation.
    #[inline]
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn set(block_ptr: *mut u64, mask: &[u64; BLOCK_WORDS]) -> bool {
        // SAFETY: `mask` is a `[u64; 8]` = 2× `__m256i`; the caller
        // guarantees `block_ptr` covers 8 readable+writable `u64`s, also
        // 2× `__m256i`. All loads/stores here are the unaligned forms.
        unsafe {
            let mp = mask.as_ptr() as *const __m256i;
            let m0 = _mm256_loadu_si256(mp);
            let m1 = _mm256_loadu_si256(mp.add(1));

            let bp = block_ptr as *mut __m256i;
            let b0 = _mm256_loadu_si256(bp as *const __m256i);
            let b1 = _mm256_loadu_si256((bp as *const __m256i).add(1));

            let eq0 = _mm256_cmpeq_epi64(_mm256_and_si256(b0, m0), m0);
            let eq1 = _mm256_cmpeq_epi64(_mm256_and_si256(b1, m1), m1);

            let combined = _mm256_and_si256(eq0, eq1);
            let bits = _mm256_movemask_epi8(combined);
            let all_set = bits == -1i32;

            _mm256_storeu_si256(bp, _mm256_or_si256(b0, m0));
            _mm256_storeu_si256(bp.add(1), _mm256_or_si256(b1, m1));

            all_set
        }
    }

    /// # Safety
    ///
    /// `block_ptr` must be valid for reads of [`BLOCK_WORDS`] consecutive
    /// `u64`s and aligned for `u64`.
    #[inline]
    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn check(block_ptr: *const u64, mask: &[u64; BLOCK_WORDS]) -> bool {
        // SAFETY: as in `set`, minus the stores.
        unsafe {
            let mp = mask.as_ptr() as *const __m256i;
            let m0 = _mm256_loadu_si256(mp);
            let m1 = _mm256_loadu_si256(mp.add(1));

            let bp = block_ptr as *const __m256i;
            let b0 = _mm256_loadu_si256(bp);
            let b1 = _mm256_loadu_si256(bp.add(1));

            let eq0 = _mm256_cmpeq_epi64(_mm256_and_si256(b0, m0), m0);
            let eq1 = _mm256_cmpeq_epi64(_mm256_and_si256(b1, m1), m1);

            let combined = _mm256_and_si256(eq0, eq1);
            _mm256_movemask_epi8(combined) == -1i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::blocked::Blocked;

    #[test]
    fn round_trip_single() {
        let b = SimdBlocked::with_capacity(64 * 1024);
        assert!(!b.check_item(0xdead_beef, 7));
        assert!(!b.set_item(0xdead_beef, 7));
        assert!(b.check_item(0xdead_beef, 7));
    }

    #[test]
    fn second_insert_reports_already_set() {
        let b = SimdBlocked::with_capacity(64 * 1024);
        b.set_item(0xdead_beef, 7);
        assert!(b.set_item(0xdead_beef, 7));
    }

    #[test]
    fn hash_zero_does_not_collapse() {
        let b = SimdBlocked::with_capacity(64 * 1024);
        b.set_item(0, 7);
        assert!(b.count_ones() >= 5);
    }

    #[test]
    fn clear_zeroes() {
        let mut b = SimdBlocked::with_capacity(64 * 1024);
        b.set_item(0xdead_beef, 7);
        b.clear();
        assert!(!b.check_item(0xdead_beef, 7));
    }

    /// SIMD output must match scalar Blocked output for the same sequence
    /// of inserts. This catches lane-ordering bugs and mis-built masks.
    #[test]
    fn matches_scalar_blocked() {
        let scalar = Blocked::with_capacity(64 * 1024);
        let simd = SimdBlocked::with_capacity(64 * 1024);

        for i in 0..1_000u64 {
            let h = i.wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let s_ret = scalar.set_item(h, 7);
            let v_ret = simd.set_item(h, 7);
            assert_eq!(s_ret, v_ret, "set_item return diverged at i={i}");
        }

        // Total popcount must match.
        assert_eq!(scalar.count_ones(), simd.count_ones());

        // Membership must match for both inserted and not-inserted hashes.
        for i in 0..2_000u64 {
            let h = i.wrapping_mul(0xbf58_476d_1ce4_e5b9);
            assert_eq!(
                scalar.check_item(h, 7),
                simd.check_item(h, 7),
                "check diverged at i={i} h={h:#x}"
            );
        }
    }

    /// Same agreement across `k` values that span the probe-word boundary
    /// (7 probes per mixed word), where the scalar lookup changes groups.
    #[test]
    fn matches_scalar_blocked_across_k() {
        for k in [1u32, 6, 7, 8, 14, 15, 20, 64] {
            let scalar = Blocked::with_capacity(16 * 1024);
            let simd = SimdBlocked::with_capacity(16 * 1024);
            for i in 0..300u64 {
                let h = i.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                assert_eq!(scalar.set_item(h, k), simd.set_item(h, k), "k={k} i={i}");
                assert!(scalar.check_item(h, k), "false negative k={k} i={i}");
            }
            assert_eq!(scalar.count_ones(), simd.count_ones(), "k={k}");
            for i in 0..2_000u64 {
                let h = i.wrapping_mul(0xbf58_476d_1ce4_e5b9);
                assert_eq!(
                    scalar.check_item(h, k),
                    simd.check_item(h, k),
                    "check diverged k={k} i={i}"
                );
            }
        }
    }

    #[test]
    fn with_capacity_floors_at_one_block() {
        assert_eq!(SimdBlocked::with_capacity(0).num_bits(), BLOCK_BITS);
        assert_eq!(SimdBlocked::with_capacity(64).num_bits(), BLOCK_BITS);
    }

    #[test]
    fn simd_blocked_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SimdBlocked>();
    }
}
