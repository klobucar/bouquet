# bouquet

> Pick your blooms.

A Bloom filter library where the storage layout and concurrency model are
*pluggable backends* behind a single ergonomic API. You pick the arrangement
you want; only that backend is compiled in.

```rust
use bouquet::{Filter, Classical};

let mut filter = Filter::<Classical>::with_false_pos(0.001).expected_items(10_000);
filter.insert(&"hello");
assert!(filter.contains(&"hello"));
```

Switch to a concurrent backend by changing one type parameter:

```rust
use bouquet::{Filter, Atomic};

let filter = Filter::<Atomic>::with_false_pos(0.001).expected_items(10_000);
// &filter.insert(...) works from multiple threads — no &mut, no locks.
```

## Why "pluggable"?

Bloom filters get rewritten constantly because the *right* layout is
workload-dependent: a textbook bit array is best when memory is the only cost,
a blocked layout is best when cache misses dominate, an atomic layout is best
when many threads want to share one filter. Most crates pick one and live with
the tradeoff.

`bouquet` makes the layout a generic parameter. Backend dispatch is
monomorphized — no `dyn`, no vtable on the bit-check loop. Backends are also
cargo features, so a project that only needs `Classical` does not pay
compile-time or binary-size cost for `Atomic`.

## Status

v0.2. Four backends:

| Backend        | Feature      | Concurrent | Notes                                                     |
|----------------|--------------|------------|-----------------------------------------------------------|
| `Classical`    | `classical`  | no         | Default. Flat `Box<[Cell<u64>]>`.                         |
| `Atomic`       | `atomic`     | yes        | `Box<[AtomicU64]>`, `Relaxed` `fetch_or`. `Sync`.         |
| `Blocked`      | `blocked`    | no         | 512-bit cache-line-sized blocks. ~constant insert cost.   |
| `SimdBlocked`  | `simd`       | no         | `Blocked` + NEON (aarch64) / AVX2 (x86_64) hot loop.      |

`SimdBlocked` selects its SIMD path at compile time. On x86_64, build with
`RUSTFLAGS="-C target-cpu=native"` (or `-C target-feature=+avx2`) to opt in
to the AVX2 path; without it, the same `SimdBlocked` type compiles to a
scalar fallback. On aarch64, NEON is mandatory in the ARMv8 spec, so it's
always selected. See [`ROADMAP.md`](./ROADMAP.md) for what's next
(`AtomicBlocked`, AVX-512, SVE).

## Feature matrix

| Feature       | Default | Effect                                                              |
|---------------|---------|---------------------------------------------------------------------|
| `std`         | yes     | Random seeding for `DefaultHasher`, `std::error::Error` impls.      |
| `classical`   | yes     | Compiles in the `Classical` backend.                                |
| `atomic`      | no      | Compiles in the `Atomic` backend (pulls in `portable-atomic`).      |
| `blocked`     | no      | Compiles in the `Blocked` backend.                                  |
| `simd`        | no      | Compiles in `SimdBlocked` (implies `blocked`).                      |
| `serde`       | no      | Validating `Serialize`/`Deserialize` for `Filter`.                  |
| `loom`        | no      | Replaces `AtomicU64` with `loom::sync::atomic::AtomicU64`.          |

`serde` and `loom` are mutually exclusive (serde would try to serialize loom's
instrumented atomic state). Enabling both is a hard compile error.

## `no_std`

Compiles without `std`; `alloc` is required. Without `std`, `DefaultHasher`
requires an explicit seed — there is no source of randomness.

```toml
bouquet = { version = "0.1", default-features = false, features = ["classical"] }
```

## Quickstart

Add the dependency:

```toml
[dependencies]
bouquet = "0.1"
```

Build a filter from a target false-positive rate and an expected item count:

```rust
use bouquet::{Filter, Classical};

let mut filter = Filter::<Classical>::with_false_pos(0.001).expected_items(1_000_000);
for item in items {
    filter.insert(&item);
}
```

Or pin the bit count directly and override the hash count:

```rust
let mut filter = Filter::<Classical>::with_num_bits(1 << 20).hashes(7);
```

Bring your own `BuildHasher`:

```rust
use foldhash::fast::RandomState;

let mut filter = Filter::<Classical>::with_num_bits(1 << 20)
    .hasher(RandomState::default())
    .hashes(7);
```

## What's deliberately *not* here

Things `bouquet` will not silently let you do, because they are real bugs that
have shipped in other crates:

- **`with_num_bits(0)`, `with_false_pos(0.0)`, `with_false_pos(1.5)`,
  `hashes(0)`** — return a typed error or panic with a clear message rather
  than producing a useless filter.
- **Double-hashing fixed point at zero** — the index derivation has been
  designed and tested so that `insert(item)` produces `k` distinct probes for
  every item, including ones that hash to `0` or `u64::MAX`.
- **Unvalidated serde deserialization** — the `Deserialize` impl rejects
  empty bit vectors, `num_hashes` over a sane cap, and inconsistent fields.
  A malicious blob cannot construct a panicking or pathologically slow filter.
- **Confusing two filters that aren't compatible** — every filter knows its
  hasher, sizing, and `k`. [`Filter::is_compatible`] returns false when any
  of those disagree, so you can guard `union` / `intersect` (planned for a
  later version) without trusting yourself to remember.

## A note on serialization

When the `serde` feature is on, the hasher state is part of the serialized
form. For the default hasher, this includes the SipHash key. **A serialized
filter is not anonymous** — anyone with the bytes can reconstruct the same
hash function and probe for membership. Treat serialized filters with the
same care as the keys themselves.

## Benchmarks

All four backends benchmarked against
[`fastbloom`](https://crates.io/crates/fastbloom) on `insert`,
`contains_present`, and `contains_absent`. Run them locally:

```sh
cargo bench --features simd
```

<!-- BENCHMARKS -->

Median time **per operation** (criterion-reported time / 64 ops per
iteration). Lower is better. Run on Apple M3 Max (`aarch64-apple-darwin`),
`cargo bench --warm-up-time 2 --measurement-time 5`.

### `insert` (ns/op)

| Size | Classical | Blocked | SimdBlocked | fastbloom |
|------|-----------|---------|-------------|-----------|
| 10K  | **9.6**   | **9.6** | 22.5        | 12.5      |
| 100K | **10.2**  | 10.8    | 23.1        | 12.6      |
| 1M   | 25.4      | 24.7    | 23.7        | **12.6**  |
| 10M  | 21.5      | 21.4    | 24.4        | **13.7**  |

### `contains_present` (ns/op)

| Size | Classical | Blocked | SimdBlocked | fastbloom |
|------|-----------|---------|-------------|-----------|
| 10K  | **10.9**  | 11.5    | 28.6        | 14.6      |
| 100K | **10.8**  | 11.4    | 28.1        | 14.6      |
| 1M   | **11.8**  | 12.3    | 29.2        | 15.0      |
| 10M  | **13.7**  | 14.2    | 29.5        | 16.8      |

### `contains_absent` (ns/op)

| Size | Classical | Blocked | SimdBlocked | fastbloom |
|------|-----------|---------|-------------|-----------|
| 10K  | 18.3      | 20.1    | 22.3        | **18.0**  |
| 100K | 18.3      | 20.2    | 22.2        | **18.0**  |
| 1M   | **21.6**  | 22.8    | 22.1        | 22.2      |
| 10M  | 24.7      | 25.1    | **22.4**    | 25.6      |

### Reading the numbers

**The "Blocked closes the insert gap" thesis half-held up.** On a non-Apple
x86 box you'd expect `Classical` to fall off a cliff at 1M (cache misses
dominate) and `Blocked` to stay near 12 ns. On the M3 Max it doesn't: the
M-series memory subsystem is aggressive enough about parallel cache misses
and prefetching that `Classical` and `Blocked` are within noise of each
other across the whole range. Both lose to fastbloom at `insert` at 1M+,
which suggests fastbloom is doing something cleverer than a generic blocked
layout — likely a smaller block tuned for a single-line touch.

**Bouquet wins at `contains_present` across every size**, by ~30% over
fastbloom. The simpler probe loop (no block math, just 7 word loads + ANDs)
beats fastbloom's per-call setup, even though fastbloom is also blocked.

**`SimdBlocked` is slower than scalar `Blocked` on aarch64.** At
`contains_present` it's ~2.5× slower, at `insert` ~2.3× slower at small
sizes (where the cache-miss component is small enough that the SIMD
overhead dominates). The current NEON implementation builds an 8-word OR
mask scalar then loads it into 4× 128-bit registers; the resulting
store-to-load-forwarding pattern doesn't pay off for a 512-bit block (= 1
cache line). It *does* edge ahead at `contains_absent`/10M, which is the
fully-cache-bound case.

The SimdBlocked NEON path needs a redesign before it's recommendable on
aarch64; see [`ROADMAP.md`](./ROADMAP.md). The x86 AVX2 path (untested in
this run) has a clearer architectural argument because the block fits in
two YMM registers instead of four NEON ones.

**Practical recommendation on Apple silicon:** use `Classical`. The
architectural distinctions matter less than expected; the simplest backend
also has the simplest hot loop, and the M-series memory subsystem makes
"flat" essentially free.

## MSRV

`1.95` — current stable. This is a brand-new crate with no installed base,
so there's no point in carrying a conservative MSRV. Pinned in CI; bumped
freely as the ecosystem moves.

## License

MIT — see [`LICENSE`](./LICENSE).
