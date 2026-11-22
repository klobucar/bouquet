//! Blocked Bloom backend: each item's `k` probes are confined to one
//! 512-bit cache-line-sized block.
//!
//! Storage is `Box<[Cell<u64>]>` like [`super::classical::Classical`], but
//! the indexing scheme is different. `set_item` and `check_item` pick one
//! 8-word block from the hash, then run all `k` probes within that block.
//! That turns the cache-miss profile of a large filter from "`k` scattered
//! line touches" into "1 line touch". Note this is the *intent*, not a
//! measured win everywhere: on Apple silicon the memory subsystem hides
//! scattered-miss latency well enough that `Classical` and `Blocked` are
//! within noise of each other across all benchmarked sizes. See the README
//! benchmarks, and weigh the accuracy cost below before switching.
//!
//! ### FP envelope
//!
//! Blocked filters have a worse false-positive rate than classical at the
//! same bits/item, because items are not distributed evenly across blocks
//! (block occupancy is Poisson, and an unlucky-heavy block dominates the
//! average). The penalty is a *multiple* of the classical rate, and the
//! multiple grows as the target tightens — at low fp each item needs many
//! bits, so few items land per block and occupancy variance dominates.
//!
//! Measured at n = 10,000 over 5M absent queries:
//!
//! | target fp | theory | `Classical` | `Blocked` | ratio |
//! |-----------|--------|-------------|-----------|-------|
//! | 1e-2 | 9.84e-3 | 9.98e-3 | 1.16e-2 | 1.2× |
//! | 1e-3 | 9.95e-4 | 9.84e-4 | 1.56e-3 | 1.6× |
//! | 1e-4 | 9.87e-5 | 9.66e-5 | 2.92e-4 | 3.0× |
//! | 1e-5 | 9.78e-6 | 1.18e-5 | 6.64e-5 | 6.8× |
//! | 1e-6 | 9.91e-7 | 2.00e-6 | 1.40e-5 | 14.1× |
//!
//! Note the rate does keep falling — it is a widening multiple, not a
//! floor. (An earlier revision of this backend made it a floor; see
//! "Probe derivation" below.)
//! Adding blocks does not help: per-block occupancy variance is independent
//! of block count.
//!
//! We do not override `optimal_num_hashes`; the classical-derived `k` is
//! near enough that the benchmark difference is negligible. Callers who
//! need to hit an exact fp should over-size by the ratio above, and callers
//! targeting below ~1e-4 should prefer `Classical` — the locality win does
//! not pay for a 3×+ accuracy loss.
//!
//! Because every probe for an item lands in a single 512-bit block, these
//! backends cannot reach arbitrarily low false-positive rates at all: once
//! the requested bits-per-item approaches the block size, one item's probes
//! cover a large fraction of its own block. `k` is therefore clamped to
//! [`MAX_BLOCK_HASHES`]; see that constant for the reasoning.
//!
//! ### Probe derivation
//!
//! Each in-block probe consumes 9 fresh bits (`log2(512)`) from a
//! SplitMix64-mixed word, seven probes per word, re-mixing with a round
//! counter when a word is exhausted.
//!
//! The obvious alternative — double hashing within the block, i.e.
//! `(h1 + i·h2) mod 512` — is what this backend originally used, and it
//! is **wrong at this scale**. Because the index is taken mod 512, only the
//! low 9 bits of `h1` and `h2` can affect the result, so the entire k-probe
//! pattern is a function of 2×9 bits. That caps the number of distinct
//! probe patterns at ~2^17 *regardless of `k` or filter size*, which puts a
//! hard floor under the false-positive rate: any query whose pattern
//! collides with an inserted item's pattern in the same block is a
//! guaranteed false positive. Measured at a 1e-6 target, the old scheme
//! delivered 9e-4 — 910× off, and unimprovable by adding memory. Drawing
//! independent bits per probe removes the floor.
//!
//! The trade is that probes are no longer guaranteed distinct within a
//! block: at `k = 7` about 4% of items get a duplicate probe (birthday
//! collision in 512 slots), very slightly reducing effective `k`. That cost
//! is on the order of a few percent of the fp rate, against a bug that cost
//! three orders of magnitude.
//!
//! ### Why 512 bits
//!
//! 512 bits = 64 bytes = one x86-64 cache line, half an Apple silicon
//! cache line (128 B). Picked for x86 portability; even at half a line on
//! Apple silicon, the locality dominates random scattered access.

use alloc::{boxed::Box, vec};
use core::cell::Cell;

use super::{Backend, Layout};

/// Block size in bits. 512 = 8 × `u64`.
pub const BLOCK_BITS: usize = 512;
const BLOCK_WORDS: usize = BLOCK_BITS / 64;
const BLOCK_BIT_MASK: u64 = (BLOCK_BITS - 1) as u64;

/// Bits of entropy one in-block probe consumes: `log2(BLOCK_BITS)` = 9.
const PROBE_BITS: u32 = BLOCK_BITS.trailing_zeros();
/// In-block probes drawn from a single 64-bit mixed word: 64 / 9 = 7.
const PROBES_PER_WORD: u32 = 64 / PROBE_BITS;

/// Odd 64-bit constant (2^64 / φ), used to decorrelate successive mixing
/// rounds.
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// Largest `k` the blocked backends will honour. Both [`Blocked`] and
/// [`super::simd_blocked::SimdBlocked`] clamp to this value.
///
/// Every probe for an item lands in one 512-bit block, so `k` is not a free
/// parameter here the way it is for a flat filter: a single insert sets up
/// to `k` of its block's 512 bits. At `k = 64` one item claims at most
/// 12.5% of its block, which still leaves room for the block to hold a
/// useful number of items. Past that, inserts saturate their own block and
/// the false-positive rate climbs toward 1 instead of falling — the
/// unclamped code answered `true` for roughly half of all absent queries at
/// `k = 532`, which [`crate::math::optimal_num_hashes`] will happily
/// recommend for a target fp around 1e-160.
///
/// Reaching the clamp at all means the requested fp is unattainable in a
/// blocked layout; use [`super::classical::Classical`] for targets that low.
pub const MAX_BLOCK_HASHES: u32 = 64;

/// Blocked Bloom backend. Plain bit array, single-threaded.
#[derive(Debug)]
pub struct Blocked {
    bits: Box<[Cell<u64>]>,
    num_bits: usize,
    num_blocks: usize,
}

impl Clone for Blocked {
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

/// Multiply-fastrange `hash → [0, num_blocks)`.
#[inline(always)]
pub(crate) fn block_index(hash: u64, num_blocks: usize) -> usize {
    ((hash as u128).wrapping_mul(num_blocks as u128) >> 64) as usize
}

/// SplitMix64 finalizer. Full avalanche, and a bijection on `u64`, so
/// distinct inputs always yield distinct mixed words.
#[inline(always)]
fn mix64(z: u64) -> u64 {
    let z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Mixed word supplying probes `round * PROBES_PER_WORD ..` for `hash`.
#[inline(always)]
fn probe_word(hash: u64, round: u64) -> u64 {
    mix64(hash.wrapping_add(round.wrapping_add(1).wrapping_mul(GOLDEN)))
}

/// Stream of in-block bit positions for one item, each in `[0, BLOCK_BITS)`.
///
/// Yields [`PROBES_PER_WORD`] probes per SplitMix64 mix, so the common
/// `k ≤ 7` case costs exactly one mix. Positions are drawn from independent
/// bit fields rather than an arithmetic progression — see the module docs
/// for why that distinction is load-bearing.
///
/// The mix is over the whole 64-bit hash, while [`block_index`] consumes
/// only the high bits through fastrange, so probe positions are effectively
/// independent of which block was selected.
#[derive(Clone, Copy)]
pub(crate) struct BlockProbes {
    hash: u64,
    word: u64,
    left: u32,
    round: u64,
}

impl BlockProbes {
    #[inline(always)]
    pub(crate) fn new(hash: u64) -> Self {
        Self {
            hash,
            word: probe_word(hash, 0),
            left: PROBES_PER_WORD,
            round: 0,
        }
    }

    /// Next in-block bit position. Always `< BLOCK_BITS`.
    #[inline(always)]
    pub(crate) fn next_probe(&mut self) -> u32 {
        if self.left == 0 {
            self.round += 1;
            self.word = probe_word(self.hash, self.round);
            self.left = PROBES_PER_WORD;
        }
        let ix = (self.word & BLOCK_BIT_MASK) as u32;
        self.word >>= PROBE_BITS;
        self.left -= 1;
        ix
    }
}

/// Clamp `k` to what a 512-bit block can usefully absorb. Applied by every
/// probe loop in both blocked backends, so `set_item`, `check_item`, and
/// the SIMD mask builder can never disagree about how many probes an item
/// has.
#[inline(always)]
pub(crate) fn effective_k(k: u32) -> u32 {
    if k > MAX_BLOCK_HASHES {
        MAX_BLOCK_HASHES
    } else {
        k
    }
}

impl Backend for Blocked {
    const LAYOUT: Layout = Layout::Blocked512;

    fn clamp_hashes(k: u32) -> u32 {
        effective_k(k)
    }

    fn with_capacity(num_bits: usize) -> Self {
        Self::try_with_capacity(num_bits).expect("Blocked::with_capacity")
    }

    fn try_with_capacity(num_bits: usize) -> Result<Self, crate::Error> {
        // Round *up* to a multiple of BLOCK_BITS, floor at one block. The
        // Filter builder passes multiples of 64, which may not be multiples
        // of 512 — and `Backend::with_capacity` is publicly callable with
        // anything, so the rounding must be unconditional: the hot loops
        // index with `get_unchecked` assuming at least one whole block.
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
        let mut probes = BlockProbes::new(hash);
        let k = effective_k(k);
        let len = self.bits.len();

        let mut all_set = true;
        for _ in 0..k {
            let in_ix = probes.next_probe() as usize;
            let word_ix = word_offset + (in_ix >> 6);
            let bit = in_ix & 63;
            debug_assert!(word_ix < len);
            // SAFETY: `block < num_blocks`, `in_ix < BLOCK_BITS`, and
            // `bits.len() == num_blocks * BLOCK_WORDS`.
            let cell = unsafe { self.bits.get_unchecked(word_ix) };
            let mask = 1u64 << bit;
            let w = cell.get();
            // Store unconditionally. Skipping the store when the bit is
            // already set is a branch on filter contents, which is a coin
            // flip while the filter fills and mispredicts accordingly.
            all_set &= w & mask != 0;
            cell.set(w | mask);
        }
        all_set
    }

    #[inline]
    fn check_item(&self, hash: u64, k: u32) -> bool {
        let block = block_index(hash, self.num_blocks);
        let word_offset = block * BLOCK_WORDS;
        let mut remaining = effective_k(k);
        let len = self.bits.len();

        // One group per mixed word: the probes inside a group are tested
        // without branching and the early exit sits between groups. Every
        // probe is in the same block, so the extra loads are L1 hits on a
        // line the first probe already fetched, and an absent item costs
        // one well-predicted branch instead of a mispredicted exit at a
        // random probe. Same probe sequence as `BlockProbes`.
        let mut round = 0u64;
        loop {
            let mut word = probe_word(hash, round);
            let n = remaining.min(PROBES_PER_WORD);
            let mut acc = 1u64;
            for _ in 0..n {
                let in_ix = (word & BLOCK_BIT_MASK) as usize;
                word >>= PROBE_BITS;
                let word_ix = word_offset + (in_ix >> 6);
                debug_assert!(word_ix < len);
                // SAFETY: see `set_item`.
                let w = unsafe { self.bits.get_unchecked(word_ix) }.get();
                acc &= w >> (in_ix & 63);
            }
            if acc & 1 == 0 {
                return false;
            }
            remaining -= n;
            if remaining == 0 {
                return true;
            }
            round += 1;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_single() {
        let b = Blocked::with_capacity(64 * 1024);
        assert!(!b.check_item(0xdead_beef, 7));
        assert!(!b.set_item(0xdead_beef, 7));
        assert!(b.check_item(0xdead_beef, 7));
    }

    #[test]
    fn second_insert_reports_already_set() {
        let b = Blocked::with_capacity(64 * 1024);
        b.set_item(0xdead_beef, 7);
        assert!(b.set_item(0xdead_beef, 7));
    }

    #[test]
    fn clear_zeroes() {
        let mut b = Blocked::with_capacity(64 * 1024);
        b.set_item(0xdead_beef, 7);
        b.clear();
        assert!(!b.check_item(0xdead_beef, 7));
        assert_eq!(b.count_ones(), 0);
    }

    #[test]
    fn hash_zero_does_not_collapse() {
        // Guard against a fixed point at zero in the blocked indexing path:
        // a hash of 0 must not produce all-zero in-block probes. This class
        // of bug has shipped in Bloom filter implementations before.
        let b = Blocked::with_capacity(64 * 1024);
        b.set_item(0, 7);
        assert!(b.count_ones() >= 5, "popcount = {}", b.count_ones());
    }

    #[test]
    fn num_bits_rounded_up_to_block() {
        // 64 bits is a legal Filter size but smaller than one block;
        // Blocked must round up to BLOCK_BITS.
        let b = Blocked::with_capacity(64);
        assert_eq!(b.num_bits(), BLOCK_BITS);
        // Degenerate sizes must floor at one block — zero blocks would make
        // the unchecked block indexing read out of bounds.
        assert_eq!(Blocked::with_capacity(0).num_bits(), BLOCK_BITS);
        assert_eq!(Blocked::with_capacity(1).num_bits(), BLOCK_BITS);
    }

    #[test]
    fn all_probes_for_one_item_share_one_block() {
        // The defining property of a blocked filter. After a single insert,
        // exactly one 512-bit block must contain all the set bits.
        let b = Blocked::with_capacity(1 << 20);
        b.set_item(0x1234_5678_9abc_def0, 7);
        let words: alloc::vec::Vec<u64> = b.bits.iter().map(|c| c.get()).collect();
        let mut nonzero_blocks = 0;
        for chunk in words.chunks(BLOCK_WORDS) {
            if chunk.iter().any(|&w| w != 0) {
                nonzero_blocks += 1;
            }
        }
        assert_eq!(nonzero_blocks, 1);
    }

    #[test]
    fn count_ones_after_many_inserts() {
        let b = Blocked::with_capacity(64 * 1024);
        for i in 0..100u64 {
            b.set_item(i.wrapping_mul(0x9e37_79b9_7f4a_7c15), 7);
        }
        let pop = b.count_ones();
        assert!(pop > 500 && pop <= 700, "popcount = {pop}");
    }

    #[test]
    fn blocked_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Blocked>();
    }
}
