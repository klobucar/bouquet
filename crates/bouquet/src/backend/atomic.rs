//! Atomic backend: textbook bit array with concurrent inserts.
//!
//! Storage is `Box<[AtomicU64]>`. Insert uses `fetch_or` with `Relaxed`
//! ordering — Bloom filter membership is monotonic (bits only ever go from
//! 0 to 1) and the contract is "may falsely report present", so cross-thread
//! happens-before edges are unnecessary. `Relaxed` lets the CPU and compiler
//! reorder freely; we still get atomicity per word, which is the only
//! guarantee the algorithm needs.
//!
//! `Atomic` is `Sync`, so `&Filter<Atomic>` can be shared across threads
//! without locks. `clear` takes `&mut self`, which by Rust's borrow rules
//! statically prevents a clear from racing with concurrent inserts.
//!
//! When the `loom` feature is active, atomics come from `loom::sync::atomic`
//! instead of `portable_atomic`. This lets the loom test in
//! `tests/loom_atomic.rs` model-check concurrent insert. `loom` and `serde`
//! are mutually exclusive features (enforced by `compile_error!` in
//! `lib.rs`).

use alloc::{boxed::Box, vec::Vec};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(feature = "loom"))]
use portable_atomic::{AtomicU64, Ordering};

use super::{double_hash, nth_index, Backend, Layout};

/// Atomic Bloom backend. `Box<[AtomicU64]>`, `Relaxed` ordering, `Sync`.
#[derive(Debug)]
pub struct Atomic {
    bits: Box<[AtomicU64]>,
    num_bits: usize,
}

impl Clone for Atomic {
    fn clone(&self) -> Self {
        let mut v = Vec::with_capacity(self.bits.len());
        for w in self.bits.iter() {
            v.push(AtomicU64::new(w.load(Ordering::Relaxed)));
        }
        Self {
            bits: v.into_boxed_slice(),
            num_bits: self.num_bits,
        }
    }
}

impl Backend for Atomic {
    const LAYOUT: Layout = Layout::Flat;

    fn with_capacity(num_bits: usize) -> Self {
        Self::try_with_capacity(num_bits).expect("Atomic::with_capacity")
    }

    fn try_with_capacity(num_bits: usize) -> Result<Self, crate::Error> {
        // Round up to a whole number of words, floor at one word. The hot
        // loops index with `get_unchecked` on the strength of
        // `num_bits == bits.len() * 64`, so this must hold for *every*
        // construction path — `Backend::with_capacity` is publicly callable,
        // not just reachable through the Filter builder.
        let num_bits = super::round_up_to(num_bits, 64)?;
        let words = num_bits / 64;
        let v = super::try_alloc_words(words, num_bits, || AtomicU64::new(0))?;
        Ok(Self {
            bits: v.into_boxed_slice(),
            num_bits,
        })
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
            debug_assert!(word < len);
            // SAFETY: `nth_index` produces a value in `[0, num_bits)` and
            // `num_bits == self.bits.len() * 64`, so `word < self.bits.len()`.
            let cell = unsafe { self.bits.get_unchecked(word) };
            let mask = 1u64 << bit;
            let prev = cell.fetch_or(mask, Ordering::Relaxed);
            if prev & mask == 0 {
                all_set = false;
            }
        }
        all_set
    }

    #[inline]
    fn check_item(&self, hash: u64, k: u32) -> bool {
        let (h1, h2) = double_hash(hash);
        let len = self.bits.len();
        // Branch-free head, early-exit tail; see `CHECK_GROUP`.
        let head = k.min(super::CHECK_GROUP);
        let mut acc = 1u64;
        for i in 0..head {
            let ix = nth_index(self.num_bits, h1, h2, i);
            let word = ix >> 6;
            let bit = ix & 63;
            debug_assert!(word < len);
            // SAFETY: see `set_item`.
            let w = unsafe { self.bits.get_unchecked(word) }.load(Ordering::Relaxed);
            acc &= w >> bit;
        }
        if acc & 1 == 0 {
            return false;
        }
        for i in head..k {
            let ix = nth_index(self.num_bits, h1, h2, i);
            let word = ix >> 6;
            let bit = ix & 63;
            debug_assert!(word < len);
            // SAFETY: see `set_item`.
            let w = unsafe { self.bits.get_unchecked(word) }.load(Ordering::Relaxed);
            if (w >> bit) & 1 == 0 {
                return false;
            }
        }
        true
    }

    fn count_ones(&self) -> usize {
        self.bits
            .iter()
            .map(|w| w.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }

    fn clear(&mut self) {
        // `&mut self` means we have unique access; a plain store is fine.
        for w in self.bits.iter() {
            w.store(0, Ordering::Relaxed);
        }
    }

    fn union_with(&mut self, other: &Self) {
        assert_eq!(self.num_bits, other.num_bits, "union_with: size mismatch");
        // `&mut self` is unique access, so load-then-store cannot lose a
        // concurrent insert into `self`. (`get_mut` would be tidier, but
        // loom's atomics do not offer it.)
        for (dst, src) in self.bits.iter().zip(other.bits.iter()) {
            let merged = dst.load(Ordering::Relaxed) | src.load(Ordering::Relaxed);
            dst.store(merged, Ordering::Relaxed);
        }
    }

    fn intersect_with(&mut self, other: &Self) {
        assert_eq!(
            self.num_bits, other.num_bits,
            "intersect_with: size mismatch"
        );
        for (dst, src) in self.bits.iter().zip(other.bits.iter()) {
            let merged = dst.load(Ordering::Relaxed) & src.load(Ordering::Relaxed);
            dst.store(merged, Ordering::Relaxed);
        }
    }

    #[cfg(feature = "serde")]
    fn snapshot_words(&self) -> Vec<u64> {
        self.bits
            .iter()
            .map(|w| w.load(Ordering::Relaxed))
            .collect()
    }

    #[cfg(feature = "serde")]
    fn from_words(words: Vec<u64>) -> Option<Self> {
        if words.is_empty() {
            return None;
        }
        let num_bits = words.len() * 64;
        let bits: Vec<AtomicU64> = words.into_iter().map(AtomicU64::new).collect();
        Some(Self {
            bits: bits.into_boxed_slice(),
            num_bits,
        })
    }
}

#[cfg(all(test, not(feature = "loom")))]
mod tests {
    use super::*;

    #[test]
    fn round_trip_single() {
        let b = Atomic::with_capacity(1024);
        assert!(!b.check_item(0xdead_beef, 7));
        assert!(!b.set_item(0xdead_beef, 7));
        assert!(b.check_item(0xdead_beef, 7));
    }

    #[test]
    fn second_insert_reports_already_set() {
        let b = Atomic::with_capacity(1024);
        b.set_item(0xdead_beef, 7);
        assert!(b.set_item(0xdead_beef, 7));
    }

    #[test]
    fn clear_zeroes() {
        let mut b = Atomic::with_capacity(1024);
        b.set_item(0xdead_beef, 7);
        assert!(b.check_item(0xdead_beef, 7));
        b.clear();
        assert!(!b.check_item(0xdead_beef, 7));
        assert_eq!(b.count_ones(), 0);
    }

    #[test]
    fn hash_zero_does_not_collapse() {
        let b = Atomic::with_capacity(64 * 1024);
        b.set_item(0, 7);
        assert!(b.count_ones() >= 5);
    }

    /// See `Classical`'s test of the same name: arbitrary sizes must be
    /// rounded up so the unchecked indexing invariant holds.
    #[test]
    fn with_capacity_rounds_up_arbitrary_sizes() {
        let b = Atomic::with_capacity(65);
        assert_eq!(b.num_bits(), 128);
        b.set_item(u64::MAX, 1);
        assert!(b.check_item(u64::MAX, 1));

        assert_eq!(Atomic::with_capacity(0).num_bits(), 64);
    }

    #[test]
    fn atomic_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Atomic>();
    }

    /// Smoke test that concurrent inserts and reads do not corrupt the bit
    /// array. This is *not* a substitute for the loom test (in
    /// `tests/loom_atomic.rs`) — it is just here to catch obvious bugs.
    #[cfg(feature = "std")]
    #[test]
    fn concurrent_inserts_smoke() {
        use std::sync::Arc;
        use std::thread;

        let f = Arc::new(Atomic::with_capacity(64 * 1024));
        let mut handles = Vec::new();
        for t in 0..4u64 {
            let f = Arc::clone(&f);
            handles.push(thread::spawn(move || {
                for i in 0..1_000u64 {
                    let v = t * 1_000_000 + i;
                    f.set_item(v.wrapping_mul(0x9e37_79b9_7f4a_7c15), 7);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        for t in 0..4u64 {
            for i in 0..1_000u64 {
                let v = t * 1_000_000 + i;
                assert!(f.check_item(v.wrapping_mul(0x9e37_79b9_7f4a_7c15), 7));
            }
        }
    }
}
