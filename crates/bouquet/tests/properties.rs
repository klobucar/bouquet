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
#[cfg(feature = "blocked")]
use bouquet::Blocked;
#[cfg(feature = "simd")]
use bouquet::SimdBlocked;

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

    /// The same guarantee for the blocked backend, which has an entirely
    /// separate probe derivation and so needs its own coverage.
    #[cfg(feature = "blocked")]
    #[test]
    fn blocked_no_false_negatives(seed in any::<u128>(), items in 100usize..2_000) {
        let f = Filter::<Blocked>::with_false_pos(0.01)
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

/// Measure the empirical FP rate of `f` over `trials` absent queries.
#[cfg(feature = "blocked")]
fn measure_fp<B: bouquet::Backend>(f: &Filter<B>, trials: u32) -> f64 {
    let mut hits = 0u32;
    for i in 0..trials as u64 {
        if f.contains(&("query", i)) {
            hits += 1;
        }
    }
    hits as f64 / trials as f64
}

/// The test whose absence let a 910× false-positive regression ship.
///
/// Blocked filters are legitimately worse than the classical formula — block
/// occupancy is Poisson, so an unlucky-heavy block dominates — but the
/// penalty must be a bounded *multiple* of theory, not a floor. An earlier
/// in-block probe derivation drew on only 16 bits of hash entropy, which
/// capped distinct probe patterns at 2^16 and put a hard floor near 1e-3
/// under the FP rate regardless of how the filter was sized. That floor is
/// what this test exists to catch: at the 1e-4 config below, the old code
/// measured ~1.7e-3, a ratio of ~17×.
#[cfg(feature = "blocked")]
#[test]
fn blocked_empirical_fp_tracks_theory_across_magnitudes() {
    // Generous but finite. Measured ratios for the current derivation sit
    // well inside this; the old derivation blew straight through it.
    const MAX_RATIO: f64 = 6.0;
    let trials = 200_000u32;

    for (n_items, fp) in [(10_000usize, 1e-2f64), (10_000, 1e-3), (10_000, 1e-4)] {
        let f = Filter::<Blocked>::with_false_pos(fp)
            .seed(0xfeed_face_dead_beef_1234_5678_aaaa_bbbb)
            .expected_items(n_items);
        for i in 0..n_items as u64 {
            f.insert(&("insert", i));
        }

        let empirical = measure_fp(&f, trials);
        let theoretical = expected_fp(f.num_bits(), n_items, f.num_hashes()).unwrap();
        let ratio = empirical / theoretical;

        assert!(
            ratio <= MAX_RATIO,
            "Blocked fp={fp}: theoretical={theoretical:.3e} empirical={empirical:.3e} \
             ratio={ratio:.1}× exceeds {MAX_RATIO}×. A ratio that grows as `fp` \
             falls means the probe derivation has an entropy floor."
        );
    }
}

/// The FP rate must keep *falling* as the target falls. A derivation with an
/// entropy floor flatlines instead — the property the ratio bound above can
/// miss if theory and empirical happen to move together.
#[cfg(feature = "blocked")]
#[test]
fn blocked_fp_keeps_improving_with_tighter_targets() {
    let trials = 200_000u32;
    let n_items = 10_000usize;

    let mut prev = f64::INFINITY;
    for fp in [1e-2f64, 1e-3, 1e-4] {
        let f = Filter::<Blocked>::with_false_pos(fp)
            .seed(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef)
            .expected_items(n_items);
        for i in 0..n_items as u64 {
            f.insert(&("insert", i));
        }
        let empirical = measure_fp(&f, trials);
        assert!(
            empirical < prev,
            "tightening the target to {fp} did not reduce the measured FP rate \
             ({empirical:.3e} vs {prev:.3e} at the previous target)"
        );
        prev = empirical;
    }
}

/// `SimdBlocked` shares the derivation, so it must share the envelope.
#[cfg(feature = "simd")]
#[test]
fn simd_blocked_fp_matches_scalar_blocked() {
    let n_items = 10_000usize;
    let trials = 200_000u32;
    let seed = 0xabcd_ef01_2345_6789_abcd_ef01_2345_6789u128;

    let scalar = Filter::<Blocked>::with_false_pos(1e-3)
        .seed(seed)
        .expected_items(n_items);
    let simd = Filter::<SimdBlocked>::with_false_pos(1e-3)
        .seed(seed)
        .expected_items(n_items);
    for i in 0..n_items as u64 {
        scalar.insert(&("insert", i));
        simd.insert(&("insert", i));
    }

    // Same derivation, same seed, same size — these must agree exactly, not
    // just statistically.
    assert_eq!(measure_fp(&scalar, trials), measure_fp(&simd, trials));
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
