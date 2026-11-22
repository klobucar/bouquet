//! Cross-backend bench: Classical vs Blocked vs SimdBlocked vs fastbloom
//! on `insert`, `contains_present`, `contains_absent`.
//!
//! The point of this bench is to show the cache-locality story across the
//! size sweep — Classical's flat layout vs Blocked's 1-line-per-op vs the
//! SIMD acceleration on the wide load–test–store.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

use bouquet::{Blocked, Classical, Filter, SimdBlocked};
use fastbloom::BloomFilter;

const SIZES: &[(usize, &str)] = &[
    (10_000, "10K"),
    (100_000, "100K"),
    (1_000_000, "1M"),
    (10_000_000, "10M"),
];
const FP: f64 = 0.01;
const QUERIES_PER_ITER: u32 = 64;

macro_rules! make_bouquet {
    ($backend:ty, $n:expr) => {
        Filter::<$backend>::with_false_pos(FP)
            .seed(0xfeed_face_cafe_babe)
            .expected_items($n)
    };
}

fn make_fastbloom(n: usize) -> BloomFilter {
    BloomFilter::with_false_pos(FP).expected_items(n)
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert");
    for &(n, name) in SIZES {
        group.throughput(Throughput::Elements(QUERIES_PER_ITER as u64));

        group.bench_with_input(BenchmarkId::new("classical", name), &n, |b, &n| {
            let f = make_bouquet!(Classical, n);
            let mut counter: u64 = 0;
            b.iter(|| {
                for _ in 0..QUERIES_PER_ITER {
                    f.insert(black_box(&counter));
                    counter = counter.wrapping_add(1);
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("blocked", name), &n, |b, &n| {
            let f = make_bouquet!(Blocked, n);
            let mut counter: u64 = 0;
            b.iter(|| {
                for _ in 0..QUERIES_PER_ITER {
                    f.insert(black_box(&counter));
                    counter = counter.wrapping_add(1);
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("simd_blocked", name), &n, |b, &n| {
            let f = make_bouquet!(SimdBlocked, n);
            let mut counter: u64 = 0;
            b.iter(|| {
                for _ in 0..QUERIES_PER_ITER {
                    f.insert(black_box(&counter));
                    counter = counter.wrapping_add(1);
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("fastbloom", name), &n, |b, &n| {
            let mut f = make_fastbloom(n);
            let mut counter: u64 = 0;
            b.iter(|| {
                for _ in 0..QUERIES_PER_ITER {
                    f.insert(black_box(&counter));
                    counter = counter.wrapping_add(1);
                }
            });
        });
    }
    group.finish();
}

fn bench_contains_present(c: &mut Criterion) {
    let mut group = c.benchmark_group("contains_present");
    for &(n, name) in SIZES {
        group.throughput(Throughput::Elements(QUERIES_PER_ITER as u64));

        group.bench_with_input(BenchmarkId::new("classical", name), &n, |b, &n| {
            let f = make_bouquet!(Classical, n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = 0;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&(counter % n as u64))) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });

        group.bench_with_input(BenchmarkId::new("blocked", name), &n, |b, &n| {
            let f = make_bouquet!(Blocked, n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = 0;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&(counter % n as u64))) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });

        group.bench_with_input(BenchmarkId::new("simd_blocked", name), &n, |b, &n| {
            let f = make_bouquet!(SimdBlocked, n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = 0;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&(counter % n as u64))) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });

        group.bench_with_input(BenchmarkId::new("fastbloom", name), &n, |b, &n| {
            let mut f = make_fastbloom(n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = 0;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&(counter % n as u64))) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });
    }
    group.finish();
}

fn bench_contains_absent(c: &mut Criterion) {
    let mut group = c.benchmark_group("contains_absent");
    for &(n, name) in SIZES {
        group.throughput(Throughput::Elements(QUERIES_PER_ITER as u64));

        group.bench_with_input(BenchmarkId::new("classical", name), &n, |b, &n| {
            let f = make_bouquet!(Classical, n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = n as u64;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&counter)) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });

        group.bench_with_input(BenchmarkId::new("blocked", name), &n, |b, &n| {
            let f = make_bouquet!(Blocked, n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = n as u64;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&counter)) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });

        group.bench_with_input(BenchmarkId::new("simd_blocked", name), &n, |b, &n| {
            let f = make_bouquet!(SimdBlocked, n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = n as u64;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&counter)) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });

        group.bench_with_input(BenchmarkId::new("fastbloom", name), &n, |b, &n| {
            let mut f = make_fastbloom(n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            let mut counter: u64 = n as u64;
            b.iter(|| {
                let mut hits = 0u32;
                for _ in 0..QUERIES_PER_ITER {
                    if f.contains(black_box(&counter)) {
                        hits += 1;
                    }
                    counter = counter.wrapping_add(1);
                }
                black_box(hits)
            });
        });
    }
    group.finish();
}

/// Fill an empty filter with `n` distinct items, start to finish.
///
/// `bench_insert` keeps inserting into one filter for the whole measurement,
/// so it mostly times a filter that is already saturated, where "is this bit
/// set?" always has the same answer. This group times the path a real
/// build takes: every bit starts clear and about half end up set, so a
/// branch on filter contents is unpredictable here and nowhere else. The
/// 10M size is left out because one fill takes over 100 ms.
fn bench_fill(c: &mut Criterion) {
    let mut group = c.benchmark_group("fill");
    for &(n, name) in &SIZES[..3] {
        group.throughput(Throughput::Elements(n as u64));

        macro_rules! fill_bouquet {
            ($label:literal, $backend:ty) => {
                group.bench_with_input(BenchmarkId::new($label, name), &n, |b, &n| {
                    let mut f = make_bouquet!($backend, n);
                    b.iter(|| {
                        f.clear();
                        for i in 0..n as u64 {
                            f.insert(black_box(&i));
                        }
                    });
                });
            };
        }
        fill_bouquet!("classical", Classical);
        fill_bouquet!("blocked", Blocked);
        fill_bouquet!("simd_blocked", SimdBlocked);

        group.bench_with_input(BenchmarkId::new("fastbloom", name), &n, |b, &n| {
            let mut f = make_fastbloom(n);
            b.iter(|| {
                f.clear();
                for i in 0..n as u64 {
                    f.insert(black_box(&i));
                }
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_insert,
    bench_fill,
    bench_contains_present,
    bench_contains_absent
);
criterion_main!(benches);
