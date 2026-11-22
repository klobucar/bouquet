//! `union_with` / `intersect_with` / `insert_all` / `Extend`, on every
//! backend.

// Uses `Filter::with_false_pos`, which is `std`-only.
#![cfg(all(feature = "classical", feature = "std"))]

use bouquet::{Backend, Classical, Error, Filter};

#[cfg(all(feature = "atomic", not(feature = "loom")))]
use bouquet::Atomic;
#[cfg(feature = "blocked")]
use bouquet::Blocked;
#[cfg(feature = "simd")]
use bouquet::SimdBlocked;

const SEED: u128 = 0xfeed_face_cafe_babe;

fn build<B: Backend>() -> Filter<B> {
    Filter::<B>::with_false_pos(0.01)
        .seed(SEED)
        .expected_items(4_000)
}

fn union_matches_inserting_both<B: Backend>() {
    let (mut a, b, both) = (build::<B>(), build::<B>(), build::<B>());
    for i in 0..1_000u64 {
        a.insert(&i);
        both.insert(&i);
    }
    for i in 1_000..2_000u64 {
        b.insert(&i);
        both.insert(&i);
    }

    let b_before = b.estimate_count();
    a.union_with(&b).unwrap();

    for i in 0..2_000u64 {
        assert!(a.contains(&i), "union lost item {i}");
    }
    // `estimate_count` is a pure function of the popcount, so equal
    // estimates plus the membership check above pin the bits down.
    assert_eq!(a.estimate_count(), both.estimate_count());
    // `other` is untouched.
    assert_eq!(b.estimate_count(), b_before);
}

fn intersect_keeps_common_items<B: Backend>() {
    let (mut a, b) = (build::<B>(), build::<B>());
    for i in 0..1_500u64 {
        a.insert(&i);
    }
    for i in 1_000..2_500u64 {
        b.insert(&i);
    }
    let before = a.estimate_count();

    a.intersect_with(&b).unwrap();

    for i in 1_000..1_500u64 {
        assert!(a.contains(&i), "intersection lost common item {i}");
    }
    assert!(a.estimate_count() < before);
    // Items only in `a` should mostly be gone. Not all: surviving bits
    // produce false positives, which is the documented caveat.
    let stale = (0..1_000u64).filter(|i| a.contains(i)).count();
    assert!(stale < 200, "{stale} of 1000 a-only items survived");
}

fn incompatible_filters_are_rejected_untouched<B: Backend>() {
    let mut a = build::<B>();
    a.insert(&1u64);
    let before = a.estimate_count();

    let other_seed = Filter::<B>::with_false_pos(0.01)
        .seed(SEED + 1)
        .expected_items(4_000);
    let other_size = Filter::<B>::with_false_pos(0.01)
        .seed(SEED)
        .expected_items(40_000);
    let other_k = Filter::<B>::with_num_bits(a.num_bits())
        .seed(SEED)
        .hashes(a.num_hashes() + 1);

    for other in [&other_seed, &other_size, &other_k] {
        other.insert(&2u64);
        assert_eq!(a.union_with(other), Err(Error::Incompatible));
        assert_eq!(a.intersect_with(other), Err(Error::Incompatible));
    }
    assert_eq!(a.estimate_count(), before);
    assert!(a.contains(&1u64));
}

fn insert_all_counts_duplicates_and_extend_inserts<B: Backend>() {
    let mut f = build::<B>();
    assert_eq!(f.insert_all(0..500u64), 0);
    // By reference, and all already present.
    let again: Vec<u64> = (0..500).collect();
    assert_eq!(f.insert_all(&again), 500);
    assert_eq!(f.insert_all(["a", "b", "a"]), 1);

    f.extend(500..600u64);
    f.extend(["c", "d"]);
    for i in 0..600u64 {
        assert!(f.contains(&i));
    }
    assert!(f.contains("c") && f.contains("d"));
}

macro_rules! for_each_backend {
    ($($name:ident => $check:ident),* $(,)?) => {$(
        #[test]
        fn $name() {
            $check::<Classical>();
            #[cfg(all(feature = "atomic", not(feature = "loom")))]
            $check::<Atomic>();
            #[cfg(feature = "blocked")]
            $check::<Blocked>();
            #[cfg(feature = "simd")]
            $check::<SimdBlocked>();
        }
    )*};
}

for_each_backend! {
    union => union_matches_inserting_both,
    intersect => intersect_keeps_common_items,
    incompatible => incompatible_filters_are_rejected_untouched,
    bulk_insert => insert_all_counts_duplicates_and_extend_inserts,
}
