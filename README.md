<h1 align="center">
  <img src="assets/readme-banner.webp" alt="Bouquet — Bloom filters for Rust &amp; Python" width="900">
</h1>

<p align="center">
  <a href="https://crates.io/crates/bouquet"><img src="https://img.shields.io/crates/v/bouquet.svg?logo=rust" alt="crates.io"></a>
  <a href="https://pypi.org/project/bouquet-bloom/"><img src="https://img.shields.io/pypi/v/bouquet-bloom?logo=pypi" alt="PyPI version"></a>
  <a href="https://docs.rs/bouquet"><img src="https://img.shields.io/docsrs/bouquet?logo=docs.rs" alt="docs.rs"></a>
  <a href="https://github.com/klobucar/bouquet/actions/workflows/ci.yml"><img src="https://github.com/klobucar/bouquet/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT licensed"></a>
  <img src="https://img.shields.io/badge/MSRV-1.95-blue.svg" alt="MSRV 1.95">
</p>

Bouquet is a Bloom filter library for Rust and Python. Use the default flat
filter for general-purpose membership checks, an atomic filter for shared
access across threads, or a blocked layout for cache locality.

## Quickstart

### Rust

```sh
cargo add bouquet
```

```rust
use bouquet::ClassicalFilter;

let filter = ClassicalFilter::with_false_pos(0.001).expected_items(10_000);
filter.insert(&"hello");
assert!(filter.contains(&"hello"));
```

The builder takes a target false-positive rate and an expected item count.
To set the size and hash count directly, use
`ClassicalFilter::with_num_bits(1 << 20).hashes(7)`. You can also pass a
custom `BuildHasher` with `.hasher(...)`.

### Python

Install `bouquet-bloom` for CPython 3.11 or later and import it as `bouquet`.

```sh
python -m pip install bouquet-bloom
```

```python
from bouquet import ClassicalFilter

bloom = ClassicalFilter.from_false_pos(0.001, 10_000)
bloom.insert("hello")
assert "hello" in bloom
assert b"hello" in bloom
```

The bindings expose `ClassicalFilter`, `AtomicFilter`, and `BlockedFilter`.
Filters accept strings and bytes-like objects and encode strings as UTF-8.
`AtomicFilter` can be shared across Python threads and releases the GIL
during bulk insertion with `insert_many`.
See the [Python bindings guide](./bouquet-py/README.md) for API details
and source builds.

## When to use a Bloom filter

A Bloom filter uses hashes to map each item to positions in a fixed-size
bit array. An unset bit rules out membership. If all the bits are set,
the item may be present.

Use a filter to avoid an expensive lookup when the item is definitely
absent, such as skipping a disk read for a missing key or bypassing a
database lookup for a new event ID. If the filter reports a possible
duplicate, check the authoritative store before discarding the event.

Memory depends on the expected item count and target false-positive rate.
A flat filter sized for 10 million items at a 1% target uses about 12 MB,
regardless of the size of each item. Inserting more items than planned
increases the false-positive rate. Bloom filters suit insertion and
membership checks. Use an exact set when you need exact membership,
individual deletions, or iteration over items.

## Rust backends

Each backend implements the same [`Filter<B, S>`](https://docs.rs/bouquet/latest/bouquet/struct.Filter.html)
API. The backend is chosen at compile time through Cargo features, and
Rust specializes the probe loop for that backend.

| Filter | Cargo feature | Shared across threads | Layout |
|--------|---------------|-----------------------|--------|
| `ClassicalFilter` | `classical` (default) | No | Flat bit array with the lowest measured false-positive rates. |
| `AtomicFilter` | `atomic` | Yes | Flat bit array with atomic updates. |
| `BlockedFilter` | `blocked` | No | 512-bit blocks for cache locality. |
| `SimdBlockedFilter` | `simd` | No | The blocked layout with NEON or AVX2 acceleration when enabled. |

Every backend supports `insert(&self)`. `AtomicFilter` also implements
`Sync`, so threads can share a reference to one filter.

### The blocked backends trade accuracy for locality

Each item probes a single 512-bit block. More heavily occupied blocks
produce more false positives, and the gap between the measured and target
rates grows as the target tightens.

| Target false-positive rate | `Classical` measured | `Blocked` measured | `Blocked` vs. target |
|----------------------------|----------------------|--------------------|----------------------|
| 1e-2      | 9.98e-3             | 1.16e-2            | 1.2×  |
| 1e-3      | 9.84e-4             | 1.56e-3            | 1.6×  |
| 1e-4      | 9.66e-5             | 2.92e-4            | 3.0×  |
| 1e-5      | 1.18e-5             | 6.64e-5            | 6.8×  |
| 1e-6      | 2.00e-6             | 1.40e-5            | 14.1× |

The measurements used 10,000 inserted items and 5 million absent queries
per configuration. For targets below roughly `1e-4`, prefer `Classical`
or size the blocked filter more conservatively and measure the resulting rate.

`Blocked` and `SimdBlocked` cap the number of probes per item at 64.
`Filter::num_hashes()` reports the actual number of probes.

`SimdBlocked` selects its SIMD path at compile time. On x86_64, build with
`RUSTFLAGS="-C target-cpu=native"` on an AVX2-capable machine, or use
`RUSTFLAGS="-C target-feature=+avx2"` to target AVX2 explicitly. Builds
without AVX2 use the scalar implementation. On aarch64, the backend uses NEON.

## Feature matrix

| Feature       | Default | Effect                                                              |
|---------------|---------|---------------------------------------------------------------------|
| `std`         | yes     | Enables builders that seed `DefaultHasher` from the OS RNG.          |
| `classical`   | yes     | Compiles in the `Classical` backend.                                |
| `atomic`      | no      | Compiles in the `Atomic` backend (pulls in `portable-atomic`).      |
| `blocked`     | no      | Compiles in the `Blocked` backend.                                  |
| `simd`        | no      | Compiles in `SimdBlocked` (implies `blocked`).                      |
| `serde`       | no      | Validating `Serialize`/`Deserialize` for `Filter`.                  |
| `loom`        | no      | Replaces `AtomicU64` with `loom::sync::atomic::AtomicU64`.          |

`serde` and `loom` are mutually exclusive.

## `no_std`

The crate can run in `no_std` environments with `alloc`. Supply an explicit
seed for `DefaultHasher` using `with_false_pos_seeded` or `with_num_bits_seeded`.

```toml
bouquet = { version = "0.1", default-features = false, features = ["classical"] }
```

```rust
use bouquet::ClassicalFilter;

// `no_std`: the seed is yours to supply.
let filter = ClassicalFilter::with_false_pos_seeded(0.001, 0x1234_5678).expected_items(10_000);
filter.insert(&"hello");
```

## Validation and serialization

Invalid sizes, hash counts, and false-positive rates produce a typed error
or a panic, depending on the constructor. `Filter::is_compatible` compares
the backend configuration, sizing, hash count, and hasher when the hasher
implements `PartialEq`.

With `serde` enabled, deserialization validates filter metadata and checks
the saved layout. `BlockedFilter` and `SimdBlockedFilter` share a layout.
Supported formats include JSON, CBOR, bincode, and postcard.
The serialization includes the hasher state, including the default
hasher's SipHash key. Treat serialized filters as sensitive data because
anyone with the bytes can reconstruct the hash function and test
membership. Load filters only from trusted sources.

## Benchmarks

The tables compare `Classical`, `Blocked`, and `SimdBlocked` with
[`fastbloom`](https://crates.io/crates/fastbloom). The `Atomic` benchmark
lives in `benches/atomic.rs`. Run the benchmarks locally with Cargo.

```sh
cargo bench --features simd
```

<!-- BENCHMARKS -->

Median time **per operation** (criterion-reported time / 64 ops per
iteration). Lower is better. Run on Apple M3 Max (`aarch64-apple-darwin`),
`cargo bench --warm-up-time 2 --measurement-time 5`.

### `insert` (ns/op)

| Size | Classical | Blocked  | SimdBlocked | fastbloom |
|------|-----------|----------|-------------|-----------|
| 10K  | 9.9       | **9.7**  | 11.3        | 9.9       |
| 100K | 10.0      | **9.7**  | 11.2        | 9.9       |
| 1M   | 10.8      | 10.6     | 11.3        | **10.4**  |
| 10M  | **11.7**  | 15.3     | 13.0        | 12.0      |

### `contains_present` (ns/op)

| Size | Classical | Blocked  | SimdBlocked | fastbloom |
|------|-----------|----------|-------------|-----------|
| 10K  | 10.8      | **10.3** | 13.1        | 11.5      |
| 100K | 10.4      | **10.2** | 13.2        | 11.6      |
| 1M   | 11.3      | **10.8** | 13.2        | 12.1      |
| 10M  | **14.1**  | 15.7     | 16.0        | **14.1**  |

### `contains_absent` (ns/op)

| Size | Classical | Blocked  | SimdBlocked | fastbloom |
|------|-----------|----------|-------------|-----------|
| 10K  | **9.2**   | 9.4      | 12.2        | 18.6      |
| 100K | **9.1**   | 9.4      | 12.0        | 18.6      |
| 1M   | 10.1      | **9.9**  | 11.8        | 22.6      |
| 10M  | **11.3**  | 11.9     | 14.4        | 26.5      |

On the M3 Max, `Classical` and `Blocked` were within about half a
nanosecond of each other up to 1M items, and `Classical` led or tied every
operation at 10M. Absent lookups took about half as long as fastbloom's at
every size. Insertions were level with fastbloom. `SimdBlocked` trailed
scalar `Blocked` by 1 to 3 ns except on 10M insertions; the SIMD results
here are for NEON. See the [roadmap](./ROADMAP.md) for backend work in
progress.

## Rust version and license

The minimum supported Rust version is 1.95. Bouquet is [MIT licensed](./LICENSE).
