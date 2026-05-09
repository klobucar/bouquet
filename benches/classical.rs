//! `bouquet::Classical` vs `fastbloom::BloomFilter` on `insert` and `contains`.
//!
//! Sized so that the four configurations span the cache hierarchy on a
//! typical x86_64 box:
//!
//! | items   | bits ≈ | size  | tier            |
//! |---------|--------|-------|-----------------|
//! | 10K     | 100K   | 12KB  | L1              |
//! | 100K    | 1M     | 120KB | L2              |
//! | 1M      | 10M    | 1.2MB | L3              |
//! | 10M     | 100M   | 12MB  | beyond L3 / RAM |
//!
//! All benches use a fixed seed so runs are reproducible.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use bouquet::{Classical, Filter};
use fastbloom::BloomFilter;

const SIZES: &[(usize, &str)] = &[
    (10_000, "10K"),
    (100_000, "100K"),
    (1_000_000, "1M"),
    (10_000_000, "10M"),
];
const FP: f64 = 0.01;
const QUERIES_PER_ITER: u32 = 64;

fn make_bouquet(n: usize) -> Filter<Classical> {
    Filter::<Classical>::with_false_pos(FP)
        .seed(0xfeed_face_cafe_babe)
        .expected_items(n)
}

fn make_fastbloom(n: usize) -> BloomFilter {
    BloomFilter::with_false_pos(FP).expected_items(n)
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert");
    for &(n, name) in SIZES {
        group.throughput(Throughput::Elements(QUERIES_PER_ITER as u64));

        group.bench_with_input(BenchmarkId::new("bouquet", name), &n, |b, &n| {
            let f = make_bouquet(n);
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

        group.bench_with_input(BenchmarkId::new("bouquet", name), &n, |b, &n| {
            let f = make_bouquet(n);
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

        group.bench_with_input(BenchmarkId::new("bouquet", name), &n, |b, &n| {
            let f = make_bouquet(n);
            for i in 0..n as u64 {
                f.insert(&i);
            }
            // Query keys past the inserted range. A small fraction will still
            // be reported present (FP ≈ 1%) but the access pattern is the
            // realistic "miss-heavy" one.
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

criterion_group!(
    benches,
    bench_insert,
    bench_contains_present,
    bench_contains_absent
);
criterion_main!(benches);
