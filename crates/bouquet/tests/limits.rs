//! Size and `k` limits: the paths where a request is too big, too small, or
//! outside what a backend can honour.
//!
//! These exist because the infallible constructors do not merely panic on an
//! oversized request — they hand an unsatisfiable size to the global
//! allocator, which calls `handle_alloc_error` and **aborts the process**.
//! An abort is not catchable from Rust and, through a language binding, kills
//! the host interpreter. The `try_*` constructors are the supported way to
//! build a filter from a size you did not choose yourself.

#![cfg(all(feature = "classical", feature = "std"))]

use bouquet::{Classical, Error, Filter};

#[cfg(feature = "blocked")]
use bouquet::Blocked;

#[test]
fn try_expected_items_reports_capacity_overflow() {
    // Astronomically large: `optimal_num_bits` rejects it rather than
    // saturating the float cast.
    let r = Filter::<Classical>::with_false_pos(1e-9).try_expected_items(usize::MAX);
    assert!(
        matches!(r, Err(Error::CapacityOverflow { .. })),
        "expected CapacityOverflow, got {:?}",
        r.map(|f| f.num_bits())
    );
}

// 64-bit only: 2^50 bits does not fit a 32-bit `usize`, and the largest size
// that does (512 MiB) is something an allocator may well satisfy.
#[cfg(target_pointer_width = "64")]
#[test]
fn try_expected_items_reports_allocation_failure() {
    // Large enough that no allocator will satisfy it, small enough that the
    // sizing math itself succeeds — so this exercises the `try_reserve`
    // path rather than the overflow guard.
    let r = Filter::<Classical>::with_num_bits(1 << 50).try_hashes(4);
    assert!(
        matches!(r, Err(Error::AllocationFailed { .. })),
        "expected AllocationFailed, got {:?}",
        r.map(|f| f.num_bits())
    );
}

#[test]
fn try_hashes_rejects_out_of_range_k_without_panicking() {
    for k in [0u32, 1025, u32::MAX] {
        let r = Filter::<Classical>::with_num_bits(1024).try_hashes(k);
        assert!(
            matches!(r, Err(Error::InvalidHashes(got)) if got == k),
            "k={k} should be rejected as InvalidHashes"
        );
    }
}

#[test]
fn try_constructors_agree_with_infallible_ones_on_valid_input() {
    let a = Filter::<Classical>::with_false_pos(0.01)
        .seed(42)
        .expected_items(10_000);
    let b = Filter::<Classical>::with_false_pos(0.01)
        .seed(42)
        .try_expected_items(10_000)
        .expect("valid size");

    assert_eq!(a.num_bits(), b.num_bits());
    assert_eq!(a.num_hashes(), b.num_hashes());
    for i in 0..1_000u32 {
        a.insert(&i);
        b.insert(&i);
    }
    for i in 0..2_000u32 {
        assert_eq!(a.contains(&i), b.contains(&i), "diverged on {i}");
    }
}

/// Blocked backends clamp `k`, and must report the clamped value — a filter
/// that says `num_hashes() == 532` while running 64 probes is lying to
/// whoever reads it back (including the serializer).
#[cfg(feature = "blocked")]
#[test]
fn blocked_reports_the_clamped_k() {
    use bouquet::backend::blocked::MAX_BLOCK_HASHES;

    let f = Filter::<Blocked>::with_num_bits(1 << 16).hashes(1024);
    assert_eq!(f.num_hashes(), MAX_BLOCK_HASHES);

    // Flat layouts are unaffected.
    let c = Filter::<Classical>::with_num_bits(1 << 16).hashes(1024);
    assert_eq!(c.num_hashes(), 1024);
}

/// The clamp is what keeps an absurdly low target from saturating every
/// block. Unclamped, this configuration answered `true` for roughly half of
/// all absent queries.
#[cfg(feature = "blocked")]
#[test]
fn blocked_does_not_saturate_at_extreme_targets() {
    let f = Filter::<Blocked>::with_false_pos(1e-160)
        .seed(1)
        .expected_items(1_000);
    for i in 0..1_000u64 {
        f.insert(&("insert", i));
    }

    let trials = 100_000u64;
    let mut hits = 0u64;
    for i in 0..trials {
        if f.contains(&("query", i)) {
            hits += 1;
        }
    }
    let fp = hits as f64 / trials as f64;
    assert!(
        fp < 0.01,
        "blocked filter saturated at an extreme target: measured FP = {fp}"
    );
}

/// Degenerate-but-legal sizes still produce a working filter.
#[test]
fn tiny_sizes_round_up_and_work() {
    for bits in [1usize, 63, 64, 65] {
        let f = Filter::<Classical>::with_num_bits(bits).seed(0).hashes(1);
        assert!(f.num_bits() >= 64);
        f.insert(&"x");
        assert!(f.contains(&"x"), "bits={bits}");
    }
}
