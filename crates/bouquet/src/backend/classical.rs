//! Classical backend: textbook bit array, single-threaded inserts.
//!
//! Storage is `Box<[Cell<u64>]>`. The `Cell` makes inserts work through
//! `&self` while keeping `Classical: !Sync`, so the borrow checker prevents
//! sharing the filter across threads — exactly the contract we want for a
//! single-threaded backend.

use alloc::{boxed::Box, vec};
use core::cell::Cell;

use super::{double_hash, nth_index, Backend};

/// Classical Bloom backend. Plain bit array, no synchronization.
#[derive(Debug)]
pub struct Classical {
    bits: Box<[Cell<u64>]>,
    num_bits: usize,
}

impl Clone for Classical {
    fn clone(&self) -> Self {
        let mut copy = vec![Cell::new(0u64); self.bits.len()].into_boxed_slice();
        for (dst, src) in copy.iter_mut().zip(self.bits.iter()) {
            dst.set(src.get());
        }
        Self {
            bits: copy,
            num_bits: self.num_bits,
        }
    }
}

impl Backend for Classical {
    fn with_capacity(num_bits: usize) -> Self {
        debug_assert!(
            num_bits >= 64 && num_bits.is_multiple_of(64),
            "Filter builder must round to a multiple of 64 ≥ 64; got {num_bits}"
        );
        let words = num_bits / 64;
        // `vec![Cell::new(0); n].into_boxed_slice()` lowers to calloc; the
        // allocator can hand us pre-zeroed pages without touching them.
        let bits = vec![Cell::new(0u64); words].into_boxed_slice();
        Self { bits, num_bits }
    }

    #[inline]
    fn num_bits(&self) -> usize {
        self.num_bits
    }

    #[inline]
    fn set_item(&self, hash: u64, k: u32) -> bool {
        let (h1, h2) = double_hash(hash);
        let len = self.bits.len();
        let mut all_set = true;
        for i in 0..k {
            let ix = nth_index(self.num_bits, h1, h2, i);
            let word = ix >> 6;
            let bit = ix & 63;
            // SAFETY: `nth_index` returns a value in `[0, num_bits)` and
            // `num_bits == self.bits.len() * 64`, so `word < self.bits.len()`.
            debug_assert!(word < len);
            let cell = unsafe { self.bits.get_unchecked(word) };
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
        let (h1, h2) = double_hash(hash);
        let len = self.bits.len();
        for i in 0..k {
            let ix = nth_index(self.num_bits, h1, h2, i);
            let word = ix >> 6;
            let bit = ix & 63;
            debug_assert!(word < len);
            // SAFETY: see `set_item`.
            let w = unsafe { self.bits.get_unchecked(word) }.get();
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
        if words.is_empty() {
            return None;
        }
        let num_bits = words.len() * 64;
        let bits: alloc::vec::Vec<Cell<u64>> = words.into_iter().map(Cell::new).collect();
        Some(Self {
            bits: bits.into_boxed_slice(),
            num_bits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_single() {
        let b = Classical::with_capacity(1024);
        assert!(!b.check_item(0xdead_beef, 7));
        let was_set = b.set_item(0xdead_beef, 7);
        assert!(!was_set);
        assert!(b.check_item(0xdead_beef, 7));
    }

    #[test]
    fn second_insert_reports_already_set() {
        let b = Classical::with_capacity(1024);
        b.set_item(0xdead_beef, 7);
        assert!(b.set_item(0xdead_beef, 7));
    }

    #[test]
    fn count_ones_matches_inserts() {
        let b = Classical::with_capacity(64 * 1024);
        for i in 0..100u64 {
            b.set_item(i.wrapping_mul(0x9e37_79b9_7f4a_7c15), 7);
        }
        let pop = b.count_ones();
        assert!(pop > 600 && pop <= 700, "popcount = {pop}");
    }

    #[test]
    fn clear_zeroes() {
        let mut b = Classical::with_capacity(1024);
        b.set_item(0xdead_beef, 7);
        assert!(b.check_item(0xdead_beef, 7));
        b.clear();
        assert!(!b.check_item(0xdead_beef, 7));
        assert_eq!(b.count_ones(), 0);
    }

    #[test]
    fn hash_zero_does_not_collapse() {
        let b = Classical::with_capacity(64 * 1024);
        b.set_item(0, 7);
        assert!(b.count_ones() >= 5);
    }

    /// Compile-time enforcement that `Classical` is `Send` (we want filters
    /// to be movable between threads) but *not* `Sync` (we don't want them
    /// shared by reference across threads — that would race on the
    /// underlying `Cell`s).
    ///
    /// The `!Sync` half is enforced by `Cell<u64>` itself, which is `!Sync`.
    /// The `Send` check is here.
    #[test]
    fn classical_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Classical>();
    }
}
