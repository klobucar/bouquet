//! Blocked Bloom backend: each item's `k` probes are confined to one
//! 512-bit cache-line-sized block.
//!
//! Storage is `Box<[Cell<u64>]>` like [`super::classical::Classical`], but
//! the indexing scheme is different. `set_item` and `check_item` pick one
//! 8-word block from the hash, then run all `k` probes within that block.
//! That turns the cache-miss profile of a large filter from "`k` scattered
//! line touches" into "1 line touch" — the architectural fix for the
//! insert-at-large-size penalty `Classical` pays.
//!
//! ### FP envelope
//!
//! Blocked filters have a slightly worse false-positive rate than classical
//! at the same bits/item, because items are not distributed perfectly evenly
//! across blocks (Poisson). The penalty is small (~10–20% at typical
//! `m/n ≥ 9`) and shrinks as the filter grows. We do not override
//! `optimal_num_hashes`; the classical-derived `k` is close enough that the
//! benchmark difference is negligible. Users targeting an exact FP rate can
//! over-size by ~5% to compensate.
//!
//! ### Why 512 bits
//!
//! 512 bits = 64 bytes = one x86-64 cache line, half an Apple silicon
//! cache line (128 B). Picked for x86 portability; even at half a line on
//! Apple silicon, the locality dominates random scattered access.

use alloc::{boxed::Box, vec};
use core::cell::Cell;

use super::{double_hash, Backend};

/// Block size in bits. 512 = 8 × `u64`.
pub const BLOCK_BITS: usize = 512;
const BLOCK_WORDS: usize = BLOCK_BITS / 64;
const BLOCK_BIT_MASK: u32 = (BLOCK_BITS - 1) as u32;

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

/// Derive a `(h1, h2)` pair for in-block double hashing, *independent* of
/// the block-selector bits. Reuses the same splitmix-style finalizer as
/// `super::double_hash`, then splits the 64-bit output into a 32-bit `h1`
/// and a 32-bit odd `h2`. Odd `h2` guarantees the cycle
/// `h1 + i·h2 (mod 512)` visits all 512 residues.
#[inline(always)]
pub(crate) fn in_block_double_hash(hash: u64) -> (u32, u32) {
    let (_, full) = double_hash(hash);
    let h1 = full as u32;
    let h2 = ((full >> 32) as u32) | 1;
    (h1, h2)
}

/// `i`-th in-block bit position, in `[0, BLOCK_BITS)`.
#[inline(always)]
pub(crate) fn nth_in_block(h1: u32, h2: u32, i: u32) -> u32 {
    h1.wrapping_add(i.wrapping_mul(h2)) & BLOCK_BIT_MASK
}

impl Backend for Blocked {
    fn with_capacity(num_bits: usize) -> Self {
        debug_assert!(num_bits >= 64 && num_bits % 64 == 0);
        // Round *up* to a multiple of BLOCK_BITS. Filter builder gives us
        // multiples of 64, which may not be multiples of 512.
        let snapped = num_bits.div_ceil(BLOCK_BITS) * BLOCK_BITS;
        let num_blocks = snapped / BLOCK_BITS;
        let words = snapped / 64;
        let bits = vec![Cell::new(0u64); words].into_boxed_slice();
        Self {
            bits,
            num_bits: snapped,
            num_blocks,
        }
    }

    #[inline]
    fn num_bits(&self) -> usize {
        self.num_bits
    }

    #[inline]
    fn set_item(&self, hash: u64, k: u32) -> bool {
        let block = block_index(hash, self.num_blocks);
        let word_offset = block * BLOCK_WORDS;
        let (h1, h2) = in_block_double_hash(hash);
        let len = self.bits.len();

        let mut all_set = true;
        for i in 0..k {
            let in_ix = nth_in_block(h1, h2, i) as usize;
            let word_ix = word_offset + (in_ix >> 6);
            let bit = in_ix & 63;
            debug_assert!(word_ix < len);
            // SAFETY: `block < num_blocks`, `in_ix < BLOCK_BITS`, and
            // `bits.len() == num_blocks * BLOCK_WORDS`.
            let cell = unsafe { self.bits.get_unchecked(word_ix) };
            let mask = 1u64 << bit;
            let w = cell.get();
            if w & mask == 0 {
                all_set = false;
                cell.set(w | mask);
            }
        }
        all_set
    }

    #[inline]
    fn check_item(&self, hash: u64, k: u32) -> bool {
        let block = block_index(hash, self.num_blocks);
        let word_offset = block * BLOCK_WORDS;
        let (h1, h2) = in_block_double_hash(hash);
        let len = self.bits.len();

        for i in 0..k {
            let in_ix = nth_in_block(h1, h2, i) as usize;
            let word_ix = word_offset + (in_ix >> 6);
            let bit = in_ix & 63;
            debug_assert!(word_ix < len);
            // SAFETY: see `set_item`.
            let w = unsafe { self.bits.get_unchecked(word_ix) }.get();
            if (w >> bit) & 1 == 0 {
                return false;
            }
        }
        true
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

    #[cfg(feature = "serde")]
    fn snapshot_words(&self) -> alloc::vec::Vec<u64> {
        self.bits.iter().map(|c| c.get()).collect()
    }

    #[cfg(feature = "serde")]
    fn from_words(words: alloc::vec::Vec<u64>) -> Option<Self> {
        if words.is_empty() || words.len() % BLOCK_WORDS != 0 {
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
        // The "fastbloom bug" verified for the blocked indexing path: a
        // hash of 0 must not produce all-zero in-block probes.
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
