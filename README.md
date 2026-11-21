<h1 align="center">bouquet</h1>

<p align="center">
  <img src="assets/logo.png" alt="bouquet logo" width="220" height="220">
</p>

<p align="center"><em>Pick your blooms.</em></p>

<!--
  The crates.io and docs.rs badges are intentionally absent until the first
  publish: `bouquet` is not on crates.io yet, so both would render as broken
  images, and the shields.io license badge reads its metadata *from*
  crates.io, so it would break too. Restore them in the release commit:

  <a href="https://crates.io/crates/bouquet"><img src="https://img.shields.io/crates/v/bouquet.svg?logo=rust" alt="crates.io"></a>
  <a href="https://docs.rs/bouquet"><img src="https://img.shields.io/docsrs/bouquet?logo=docs.rs" alt="docs.rs"></a>
-->
<p align="center">
  <a href="https://github.com/klobucar/bouquet/actions/workflows/ci.yml"><img src="https://github.com/klobucar/bouquet/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT licensed"></a>
  <img src="https://img.shields.io/badge/MSRV-1.95-blue.svg" alt="MSRV 1.95">
</p>

A Bloom filter library where the storage layout and concurrency model are
*pluggable backends* behind a single ergonomic API. You pick the arrangement
you want; only that backend is compiled in.

```rust
use bouquet::ClassicalFilter;

let filter = ClassicalFilter::with_false_pos(0.001).expected_items(10_000);
filter.insert(&"hello");
assert!(filter.contains(&"hello"));
```

Note `insert` takes `&self` on *every* backend — the filters use interior
mutability throughout, so no binding needs to be `mut`. What the concurrent
backend adds is `Sync`: only `AtomicFilter` may be shared across threads.

```rust
use bouquet::AtomicFilter;

let filter = AtomicFilter::with_false_pos(0.001).expected_items(10_000);
// Unlike the others, `&filter` can cross a thread boundary — no locks.
// `ClassicalFilter` is `!Sync`, so the borrow checker rejects that at
// compile time rather than letting you race.
```

## What's a Bloom filter, and when do you want one?

A **Bloom filter** is a tiny probabilistic membership data structure: a
fixed bit array plus `k` hash functions. To insert `x`, hash it `k` ways
and set those bits. To query `x`, hash it `k` ways and check those bits —
if any is unset, `x` is *definitely not* present; if all are set, `x` is
*probably* present.

That's it. The whole structure is a bit array.

**The contract:**

- **No false negatives.** If you inserted `x`, `contains(x)` returns true.
- **False positives are bounded.** Some non-inserted items look present;
  the rate is configurable (1%, 0.01%, whatever) and trades off against
  memory.
- **Memory is independent of item size.** A filter for 10M items at 1% FP
  fits in ~12 MB whether each item is a 4-byte integer or a 4-KB document.
  ~1.2 bytes per item, full stop.

**You want one when the exact answer is expensive but a "probably-not"
cheaply rules out most queries:**

- *Dedupe before a write.* "Have we seen this URL / event ID / hash
  before?" — check the filter, skip the database INSERT if it says yes.
- *Negative caching.* Gate an auth check / DNS lookup / CDN origin fetch
  on a local filter; if the filter says no, skip the round trip.
- *LSM-tree reads.* RocksDB and friends keep a per-SSTable Bloom filter so
  a key lookup can skip files that definitely don't contain it.
- *Bioinformatics.* Short-read alignment uses Bloom filters to throw out
  candidate positions before doing exact comparison.

The mental model: **let the filter say "no" cheaply for the common case;
only do the expensive lookup when it says "maybe."**

**You do *not* want one when:**

- You need to **enumerate** the contents. Bloom filters store no items,
  only bits — there's nothing to iterate.
- You need **deletion**. Basic Bloom is insert-only. Counting variants
  exist but cost 4–8× the memory; not in `bouquet` v0.x.
- The dataset is **small enough to fit in a `HashSet`**. Below ~10K items,
  the constant cost of `k` hashes and `k` bit lookups isn't worth the
  memory savings.
- You need an **exact** answer. False positives are part of the contract;
  use `HashSet` / `BTreeSet`.

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

v0.1. Four backends:

| Backend        | Feature      | Concurrent | Notes                                                     |
|----------------|--------------|------------|-----------------------------------------------------------|
| `Classical`    | `classical`  | no         | Default. Flat `Box<[Cell<u64>]>`. Best accuracy.          |
| `Atomic`       | `atomic`     | yes        | `Box<[AtomicU64]>`, `Relaxed` `fetch_or`. `Sync`.         |
| `Blocked`      | `blocked`    | no         | 512-bit cache-line-sized blocks. **Costs accuracy** — see below. |
| `SimdBlocked`  | `simd`       | no         | `Blocked` + NEON (aarch64) / AVX2 (x86_64) hot loop. Same accuracy cost. |

### The blocked backends trade accuracy for locality

This is not a free swap. Confining an item's probes to one 512-bit block
means block occupancy is Poisson-distributed, and the heavy blocks dominate
the average — so the measured false-positive rate is a *multiple* of what
you asked for, and the multiple grows as the target tightens:

| target fp | `Classical` measured | `Blocked` measured | ratio |
|-----------|---------------------|--------------------|-------|
| 1e-2      | 9.98e-3             | 1.16e-2            | 1.2×  |
| 1e-3      | 9.84e-4             | 1.56e-3            | 1.6×  |
| 1e-4      | 9.66e-5             | 2.92e-4            | 3.0×  |
| 1e-5      | 1.18e-5             | 6.64e-5            | 6.8×  |
| 1e-6      | 2.00e-6             | 1.40e-5            | 14.1× |

(n = 10,000; 5M absent queries per configuration.) Adding memory does not
close the gap — per-block occupancy variance is independent of block count.
Either over-size by the ratio above, or use `Classical` below ~1e-4.

`Blocked` and `SimdBlocked` also cap `k` at 64
(`backend::blocked::MAX_BLOCK_HASHES`), since a single item's probes cannot
usefully cover more than a fraction of its own 512-bit block.
`Filter::num_hashes()` reports the clamped value.

`SimdBlocked` selects its SIMD path at compile time. On x86_64, build with
`RUSTFLAGS="-C target-cpu=native"` (or `-C target-feature=+avx2`) to opt in
to the AVX2 path; without it, the same `SimdBlocked` type compiles to a
scalar fallback. On aarch64, NEON is mandatory in the ARMv8 spec, so it's
always selected. See [`ROADMAP.md`](./ROADMAP.md) for what's next
(`AtomicBlocked`, AVX-512, SVE).

## Feature matrix

| Feature       | Default | Effect                                                              |
|---------------|---------|---------------------------------------------------------------------|
| `std`         | yes     | Random seeding for `DefaultHasher`; the unseeded builder entry points. |
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

Note the constructor names differ — `with_false_pos` / `with_num_bits` are
`std`-only, because they seed the hasher from the OS RNG:

```rust
use bouquet::ClassicalFilter;

// `no_std`: the seed is yours to supply.
let filter = ClassicalFilter::with_false_pos_seeded(0.001, 0x1234_5678).expected_items(10_000);
filter.insert(&"hello");
```

## Quickstart

Add the dependency:

```toml
[dependencies]
bouquet = "0.1"
```

Build a filter from a target false-positive rate and an expected item count:

```rust
use bouquet::ClassicalFilter;

let filter = ClassicalFilter::with_false_pos(0.001).expected_items(1_000_000);
for item in items {
    filter.insert(&item);
}
```

Or pin the bit count directly and override the hash count:

```rust
let filter = ClassicalFilter::with_num_bits(1 << 20).hashes(7);
```

Bring your own `BuildHasher`:

```rust
use foldhash::fast::RandomState;

let filter = ClassicalFilter::with_num_bits(1 << 20)
    .hasher(RandomState::default())
    .hashes(7);
```

The generic [`Filter<B, S>`](https://docs.rs/bouquet/latest/bouquet/struct.Filter.html)
is still the underlying type — the per-backend names
(`ClassicalFilter`, `AtomicFilter`, `BlockedFilter`, `SimdBlockedFilter`)
are just one-line type aliases. Generic code over backends still uses
`Filter<B, S>` directly.

## What's deliberately *not* here

Things `bouquet` will not silently let you do, because they are real bugs that
have shipped in other crates:

- **`with_num_bits(0)`, `with_false_pos(0.0)`, `with_false_pos(1.5)`,
  `hashes(0)`** — return a typed error or panic with a clear message rather
  than producing a useless filter.
- **Double-hashing fixed point at zero** — some implementations have shipped
  a derivation where `hash == 0` collapses every probe onto bit 0. Ours is
  tested against that for `0` and `u64::MAX` on every backend. (Note this is
  *not* a promise of `k` distinct probes: the flat backends use multiplicative
  fastrange, which can repeat a position — at `k = 7`, `m = 1024`, about 0.5%
  of items get a duplicate. That costs a little accuracy and is priced into
  the measured rates above; it is not a correctness bug.)
- **Unvalidated serde deserialization** — the `Deserialize` impl rejects
  empty bit vectors, `num_hashes` over a sane cap, and inconsistent fields.
  A malicious blob cannot construct a panicking or pathologically slow filter.
- **Confusing two filters that aren't compatible** — every filter knows its
  hasher, sizing, and `k`. `Filter::is_compatible` returns false when any
  of those disagree, so you can guard `union` / `intersect` (planned for a
  later version) without trusting yourself to remember. It requires
  `S: BuildHasher + PartialEq`, which the default hasher satisfies but a
  third-party one may not.

## A note on serialization

When the `serde` feature is on, the hasher state is part of the serialized
form. For the default hasher, this includes the SipHash key. **A serialized
filter is not anonymous** — anyone with the bytes can reconstruct the same
hash function and probe for membership. Treat serialized filters with the
same care as the keys themselves.

This matters more for the blocked backends than you might expect. `Blocked`
picks an item's 512-bit block from the top `log2(num_blocks)` hash bits, so
an attacker who knows the key needs only ~`num_blocks` trial hashes — not
2^64 — to steer chosen items into one block. On a 1 MiB filter that is ~2,000
hashes per item; 4,000 crafted items make every query mapping to that block a
false positive while the filter still reads 0.05% full, so occupancy
monitoring will not notice. Do not deserialize filters from untrusted
sources, and do not publish serialized filters you still rely on.

The serialized form carries a `layout` tag identifying the backend family
that wrote it. Loading a `Filter<Classical>` blob as a `Filter<Blocked>` is
rejected rather than silently producing false negatives; `Blocked` and
`SimdBlocked` share a layout and remain interchangeable. Both self-describing
formats (JSON, CBOR) and non-self-describing ones (bincode, postcard) round
trip.

## Benchmarks

`Classical`, `Blocked`, and `SimdBlocked` benchmarked against
[`fastbloom`](https://crates.io/crates/fastbloom) on `insert`,
`contains_present`, and `contains_absent`. `Atomic` has a bench target
(`benches/atomic.rs`) but no published numbers — its cost is dominated by
contention, which a single-threaded harness does not measure. Run them
locally:

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

**Bouquet wins at `contains_present` across every size**, by 18–25% over
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

`1.95`. This is a brand-new crate with no installed base, so there's no
point in carrying a conservative MSRV. Pinned in CI; bumped freely as the
ecosystem moves.

## License

MIT — see [`LICENSE`](./LICENSE).
