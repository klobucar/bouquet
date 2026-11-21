//! Closed-form sizing formulas for Bloom filters.
//!
//! All inputs are validated; pathological values (zero items, fp ≤ 0, fp ≥ 1)
//! return `None` rather than producing nonsense.

use libm::{ceil, exp, log, pow};

const LN_2: f64 = core::f64::consts::LN_2;
const LN_2_SQUARED: f64 = LN_2 * LN_2;

/// Maximum number of hashes the library will ever recommend or accept.
///
/// Above this value the per-query cost dominates and the marginal gain in
/// false-positive rate is negligible. We also enforce this cap when
/// deserializing so that a hostile blob cannot make every query
/// pathologically slow.
pub const MAX_HASHES: u32 = 1024;

/// Smallest filter we will ever allocate (in bits).
///
/// Sizes near zero hit edge cases in the index derivation and the analytical
/// formulas, and are useless in practice. One `u64` word is the floor.
pub const MIN_BITS: usize = 64;

/// Optimal bit count for a filter holding `num_items` with target false
/// positive rate `fp`.
///
/// `m = -n · ln(p) / (ln 2)²`, rounded up to the nearest `u64` word.
///
/// Returns `None` if `num_items == 0`, `fp <= 0.0`, `fp >= 1.0`, `fp` is
/// NaN, or the computed size cannot be rounded up to a whole word without
/// overflowing `usize`.
pub fn optimal_num_bits(num_items: usize, fp: f64) -> Option<usize> {
    if num_items == 0 || !fp.is_finite() || fp <= 0.0 || fp >= 1.0 {
        return None;
    }
    let n = num_items as f64;
    let m = ceil(-(n * log(fp)) / LN_2_SQUARED);
    // Without this guard the `as usize` cast saturates at `usize::MAX` and
    // `round_up_to_word` then wraps to 0 in release builds. `usize::MAX / 2`
    // bits (~1.2 EB on 64-bit) is far beyond anything allocatable, so the
    // cap costs nothing real.
    if m >= (usize::MAX / 2) as f64 {
        return None;
    }
    let m = m as usize;
    Some(round_up_to_word(m.max(MIN_BITS)))
}

/// Optimal number of hash functions for a filter of `num_bits` bits holding
/// `num_items`.
///
/// `k = (m/n) · ln 2`, rounded to the nearest integer, clamped to
/// `[1, MAX_HASHES]`.
///
/// Returns `None` if `num_bits == 0` or `num_items == 0`.
pub fn optimal_num_hashes(num_bits: usize, num_items: usize) -> Option<u32> {
    if num_bits == 0 || num_items == 0 {
        return None;
    }
    let m = num_bits as f64;
    let n = num_items as f64;
    let k = (m / n) * LN_2;
    // Clamp *before* narrowing. `rounded.max(1) as u32` truncates modulo
    // 2^32, so a true optimum of exactly 2^32 used to narrow to 0 — a k=0
    // filter, which sets nothing on insert and answers `true` for every
    // query. Clamping in `i64` keeps the documented `[1, MAX_HASHES]`
    // postcondition for every input.
    let rounded = libm::round(k) as i64;
    Some(rounded.clamp(1, MAX_HASHES as i64) as u32)
}

/// Expected false-positive rate for a filter of `num_bits` bits holding
/// `num_items` after all `k = num_hashes` probes.
///
/// `(1 - e^{-kn/m})^k`. Returns `None` for invalid inputs.
pub fn expected_fp(num_bits: usize, num_items: usize, num_hashes: u32) -> Option<f64> {
    if num_bits == 0 || num_items == 0 || num_hashes == 0 {
        return None;
    }
    let m = num_bits as f64;
    let n = num_items as f64;
    let k = num_hashes as f64;
    let inner = 1.0 - exp(-k * n / m);
    Some(pow(inner, k))
}

fn round_up_to_word(num_bits: usize) -> usize {
    const W: usize = 64;
    num_bits.div_ceil(W) * W
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: f64, expected: f64, tol: f64) {
        let rel = (actual - expected).abs() / expected.abs().max(1e-12);
        assert!(
            rel <= tol,
            "expected {expected}, got {actual} (rel err {rel})"
        );
    }

    #[test]
    fn sizing_textbook_values() {
        // For 1M items at 1% FP, the textbook size is ~9.585 bits/item.
        let m = optimal_num_bits(1_000_000, 0.01).unwrap();
        approx(m as f64, 9_585_058.0, 0.001);

        let k = optimal_num_hashes(m, 1_000_000).unwrap();
        assert_eq!(k, 7);
    }

    #[test]
    fn sizing_round_trips_across_magnitudes() {
        for items in [100, 10_000, 1_000_000, 100_000_000] {
            for fp in [0.1, 0.01, 0.001, 1e-6, 1e-9] {
                let Some(m) = optimal_num_bits(items, fp) else {
                    // Only a 32-bit `usize` can run out of room here (e.g.
                    // 1e8 items at 1e-9 needs ~4.3e9 bits).
                    assert!(usize::BITS < 64, "items={items} fp={fp} must fit");
                    continue;
                };
                let k = optimal_num_hashes(m, items).expect("valid");
                let predicted = expected_fp(m, items, k).expect("valid");
                // Predicted FP at the optimal k should be ≤ target FP, since
                // we round bits *up*.
                assert!(
                    predicted <= fp * 1.05,
                    "items={items} fp={fp} m={m} k={k} predicted={predicted}"
                );
            }
        }
    }

    #[test]
    fn rejects_invalid_inputs() {
        assert!(optimal_num_bits(0, 0.01).is_none());
        assert!(optimal_num_bits(100, 0.0).is_none());
        assert!(optimal_num_bits(100, 1.0).is_none());
        assert!(optimal_num_bits(100, 1.5).is_none());
        assert!(optimal_num_bits(100, -0.1).is_none());
        assert!(optimal_num_bits(100, f64::NAN).is_none());

        assert!(optimal_num_hashes(0, 100).is_none());
        assert!(optimal_num_hashes(100, 0).is_none());

        assert!(expected_fp(0, 100, 7).is_none());
        assert!(expected_fp(100, 0, 7).is_none());
        assert!(expected_fp(100, 100, 0).is_none());
    }

    #[test]
    fn rejects_overflowing_sizes() {
        // A size whose bit count cannot fit in usize must be rejected, not
        // saturate-then-wrap to 0 (which previously fed a zero-word
        // allocation to the backends in release builds).
        assert!(optimal_num_bits(usize::MAX, 1e-9).is_none());
        assert!(optimal_num_bits(usize::MAX / 2, 1e-12).is_none());
    }

    #[test]
    fn min_bits_floor() {
        let m = optimal_num_bits(1, 0.99).unwrap();
        assert!(m >= MIN_BITS);
    }

    #[test]
    fn hashes_clamped_to_max() {
        // Pathologically over-provisioned: m huge, n tiny.
        let k = optimal_num_hashes(usize::MAX / 2, 1).unwrap();
        assert!(k <= MAX_HASHES);
    }

    /// Regression: the clamp used to narrow to `u32` before bounding, so
    /// optima at or just above 2^32 wrapped. (64-bit only: these bit counts
    /// do not fit a 32-bit `usize`.) `6_196_328_018` bits for one
    /// item has a true optimum of ~4.29e9 hashes; it must clamp to
    /// `MAX_HASHES`, never to 0 or to some small wrapped remainder.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn hashes_clamp_before_narrowing() {
        for &(m, n) in &[
            (6_196_328_018usize, 1usize),
            (6_196_328_064, 1),
            (6_196_328_018_048, 1000),
        ] {
            let k = optimal_num_hashes(m, n).expect("valid inputs");
            assert_eq!(k, MAX_HASHES, "m={m} n={n} produced k={k}");
        }
    }

    /// The documented postcondition, swept across the 2^32 boundary where
    /// the truncation bug lived.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn hashes_always_within_documented_range() {
        // `k = m/n * ln2`, so sweeping `m` around `2^32 / ln2` sweeps the
        // computed `k` across the 2^32 boundary where the cast wrapped.
        let base = ((1u64 << 32) as f64 / LN_2) as usize;
        for delta in 0..2_000usize {
            let m = base.saturating_add(delta);
            let k = optimal_num_hashes(m, 1).expect("valid inputs");
            assert!(
                (1..=MAX_HASHES).contains(&k),
                "m={m} produced out-of-range k={k}"
            );
        }
    }

    #[test]
    fn hashes_at_least_one() {
        // Pathologically under-provisioned: m tiny, n huge.
        let k = optimal_num_hashes(64, 1_000_000).unwrap();
        assert_eq!(k, 1);
    }

    #[test]
    fn round_up_to_word_works() {
        assert_eq!(round_up_to_word(1), 64);
        assert_eq!(round_up_to_word(63), 64);
        assert_eq!(round_up_to_word(64), 64);
        assert_eq!(round_up_to_word(65), 128);
    }
}
