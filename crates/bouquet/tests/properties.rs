//! Property tests:
//!
//! 1. Every inserted item is reported as contained (no false negatives).
//! 2. The empirical false-positive rate is within ~2× of the analytical
//!    rate predicted by `bouquet::math::expected_fp` across multiple sizes.
//!
//! The 2× tolerance is generous on purpose. The analytical formula assumes
//! independent uniformly-random hashes, and double-hashing approximates that
//! but isn't exact. 2× is wide enough to absorb finite-sample noise without
//! masking a real regression — anything wider would.

// Uses `Filter::with_false_pos`, which is `std`-only.
#![cfg(all(feature = "classical", feature = "std"))]

use bouquet::math::expected_fp;
use bouquet::{Classical, Filter};
use proptest::prelude::*;

#[cfg(feature = "atomic")]
use bouquet::Atomic;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// No false negatives: every item we inserted must be reported as present.
    #[test]
    fn no_false_negatives(seed in any::<u128>(), items in 100usize..2_000) {
        let f = Filter::<Classical>::with_false_pos(0.01)
            .seed(seed)
            .expected_items(items);
        let inserted: Vec<u64> = (0..items as u64).map(|i| i.wrapping_mul(0x9e37_79b9_7f4a_7c15)).collect();
        for x in &inserted {
            f.insert(x);
        }
        for x in &inserted {
            prop_assert!(f.contains(x), "false negative for {}", x);
        }
    }
}

/// Empirical FP rate within 2× of theory across several `(items, fp)` configs.
///
/// Not a `proptest` because proptest's shrinking model fights with
/// statistical sampling. Run a handful of fixed configurations with enough
/// trials that the binomial standard error is well under our 2× envelope.
#[test]
fn empirical_fp_matches_theory() {
    let configs = [
        (1_000usize, 0.05f64),
        (1_000, 0.01),
        (10_000, 0.01),
        (10_000, 0.001),
    ];
    let trials = 100_000u32;

    for (n_items, fp) in configs {
        let f = Filter::<Classical>::with_false_pos(fp)
            .seed(0xfeed_face_dead_beef_1234_5678_aaaa_bbbb)
            .expected_items(n_items);

        // Insert items keyed by `(insert, i)`; query uses `(query, i)`.
        // Different domains so a query is guaranteed not to be one of the
        // inserted values.
        for i in 0..n_items as u64 {
            f.insert(&("insert", i));
        }

        let mut hits = 0u32;
        for i in 0..trials as u64 {
            if f.contains(&("query", i)) {
                hits += 1;
            }
        }
        let empirical = hits as f64 / trials as f64;
        let theoretical = expected_fp(f.num_bits(), n_items, f.num_hashes()).unwrap();

        // 2× either direction. Tighter floor for tiny `fp` so a few stray
        // hits don't trip it.
        let lower = (theoretical / 2.0).max(theoretical - 0.005);
        let upper = (theoretical * 2.0).max(theoretical + 0.005);
        assert!(
            empirical >= lower && empirical <= upper,
            "items={n_items} fp={fp}: theoretical={theoretical:.4} empirical={empirical:.4} \
             (allowed [{lower:.4}, {upper:.4}])"
        );
    }
}

#[cfg(feature = "atomic")]
#[test]
fn atomic_empirical_fp_matches_theory() {
    // Sanity: same property holds for the atomic backend with the same hash
    // function, since the index derivation is shared.
    let f = Filter::<Atomic>::with_false_pos(0.01)
        .seed(0x1234_5678_dead_beef)
        .expected_items(10_000);
    for i in 0..10_000u64 {
        f.insert(&("insert", i));
    }
    let trials = 100_000u32;
    let mut hits = 0u32;
    for i in 0..trials as u64 {
        if f.contains(&("query", i)) {
            hits += 1;
        }
    }
    let empirical = hits as f64 / trials as f64;
    let theoretical = expected_fp(f.num_bits(), 10_000, f.num_hashes()).unwrap();
    assert!(
        empirical >= theoretical / 2.0 - 0.005 && empirical <= theoretical * 2.0 + 0.005,
        "atomic FP: theoretical={theoretical:.4} empirical={empirical:.4}"
    );
}
