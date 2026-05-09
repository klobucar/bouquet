//! `bouquet::Atomic` insert / contains, single-threaded vs concurrent.
//!
//! Two questions this bench answers:
//!
//! 1. How much does the atomic backend cost on a single thread vs
//!    `Classical`? `fetch_or` should be only marginally slower than a plain
//!    OR, but the hot loop is the only thing that matters.
//! 2. Does concurrent insert scale? With four threads hammering the same
//!    filter, the locked-bus penalty should still beat a `Mutex<Classical>`.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use bouquet::{Atomic, Classical, Filter};

const SIZES: &[(usize, &str)] = &[(10_000, "10K"), (100_000, "100K"), (1_000_000, "1M")];
const FP: f64 = 0.01;
const QUERIES_PER_ITER: u32 = 64;

fn make_atomic(n: usize) -> Filter<Atomic> {
    Filter::<Atomic>::with_false_pos(FP)
        .seed(0xfeed_face_cafe_babe)
        .expected_items(n)
}

fn make_classical(n: usize) -> Filter<Classical> {
    Filter::<Classical>::with_false_pos(FP)
        .seed(0xfeed_face_cafe_babe)
        .expected_items(n)
}

fn bench_single_threaded_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("st_insert");
    for &(n, name) in SIZES {
        group.throughput(Throughput::Elements(QUERIES_PER_ITER as u64));

        group.bench_with_input(BenchmarkId::new("atomic", name), &n, |b, &n| {
            let f = make_atomic(n);
            let mut counter: u64 = 0;
            b.iter(|| {
                for _ in 0..QUERIES_PER_ITER {
                    f.insert(black_box(&counter));
                    counter = counter.wrapping_add(1);
                }
            });
        });

        group.bench_with_input(BenchmarkId::new("classical", name), &n, |b, &n| {
            let f = make_classical(n);
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

fn bench_single_threaded_contains(c: &mut Criterion) {
    let mut group = c.benchmark_group("st_contains");
    for &(n, name) in SIZES {
        group.throughput(Throughput::Elements(QUERIES_PER_ITER as u64));

        group.bench_with_input(BenchmarkId::new("atomic", name), &n, |b, &n| {
            let f = make_atomic(n);
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

        group.bench_with_input(BenchmarkId::new("classical", name), &n, |b, &n| {
            let f = make_classical(n);
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

/// Concurrent insert: four threads pushing into the same `&Filter<Atomic>`.
/// Bench measures wall-clock to insert a fixed number of items per thread.
fn bench_concurrent_insert(c: &mut Criterion) {
    use std::sync::Arc;
    use std::thread;

    const PER_THREAD: u64 = 10_000;
    const THREADS: u64 = 4;

    let mut group = c.benchmark_group("concurrent_insert_4t");
    group.throughput(Throughput::Elements(PER_THREAD * THREADS));

    let n = 1_000_000usize;
    group.bench_with_input(BenchmarkId::new("atomic", "1M"), &n, |b, &n| {
        b.iter_batched(
            || Arc::new(make_atomic(n)),
            |f| {
                let handles: Vec<_> = (0..THREADS)
                    .map(|t| {
                        let f = Arc::clone(&f);
                        thread::spawn(move || {
                            for i in 0..PER_THREAD {
                                f.insert(&(t * 1_000_000 + i));
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    h.join().unwrap();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_single_threaded_insert,
    bench_single_threaded_contains,
    bench_concurrent_insert
);
criterion_main!(benches);
