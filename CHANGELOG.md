# Changelog

All notable changes to this project are documented here. This project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html); pre-1.0, minor
versions may break.

## [0.1.1] — 2026-11-21

Test and benchmark maintenance. **No library code changed** since 0.1.0; the
code under `src/` is functionally identical, so there is nothing to upgrade
for.

- Tests that drive `Atomic` from plain threads
  (`atomic_filter_concurrent_insert`, `atomic_empirical_fp_matches_theory`)
  are no longer built under the `loom` feature, where loom's atomics panic
  outside a `loom::model`. A bare `cargo test --features loom,atomic` now
  passes; model-checked coverage stays in `tests/loom_atomic.rs`.
- Benchmarks use `std::hint::black_box` instead of the deprecated
  `criterion::black_box`.
- Dev-dependencies updated (criterion 0.8, fastbloom 0.17, rand 0.10,
  foldhash 0.2).

## [0.1.0] — 2026-11-21

Initial release. Four Bloom filter backends behind one generic API:
`Classical` (flat), `Atomic` (concurrent, `Sync`), `Blocked` (512-bit
cache-line blocks), and `SimdBlocked` (`Blocked` with NEON / AVX2 hot loops).
Optional `serde`, `no_std` support with `alloc`, and Python bindings
(`bouquet-bloom`, imported as `bouquet`).

### Design decisions worth knowing about

These are the non-obvious ones — the places where the straightforward
implementation is wrong, or where the API deliberately refuses something.

- **Blocked backends trade accuracy for locality, and the trade is steep at
  low false-positive targets.** Confining an item's probes to one block makes
  block occupancy Poisson-distributed, so the measured rate is a multiple of
  the target: ~1.2× at 1e-2, rising to ~14× at 1e-6. Adding memory does not
  close the gap. The README status table carries measured numbers; prefer
  `Classical` below ~1e-4.

- **In-block probes are drawn from independent bit fields, not an arithmetic
  progression.** The textbook `(h1 + i·h2) mod 512` is wrong at this scale:
  taking the index mod 512 means only the low 9 bits of each of `h1`/`h2`
  reach the result, capping distinct probe patterns at ~2^16 regardless of `k`
  or filter size — a hard false-positive floor near 1e-3 that no amount of
  memory can lower. Probes instead consume 9 fresh bits each from a
  SplitMix64-mixed word, seven per mix.

- **`k` is clamped to 64 on blocked backends**
  (`backend::blocked::MAX_BLOCK_HASHES`). Every probe lands in one 512-bit
  block, so a large `k` saturates the block rather than sharpening the filter.
  `Filter::num_hashes()` reports the clamped value, not the request.

- **Serialized filters carry a `layout` tag.** A bare word array does not say
  which probe scheme wrote it, and a flat blob's word count satisfies the
  blocked backends' checks — so without a tag it would deserialize
  *successfully* into the wrong backend and then return false negatives for
  every item it contained. Mismatches are rejected with
  `Error::LayoutMismatch`. `Blocked` and `SimdBlocked` share a layout and are
  interchangeable.

- **Both fallible and infallible constructors.** `try_expected_items` /
  `try_hashes` / `Backend::try_with_capacity` reserve through
  `Vec::try_reserve_exact` and return `Error::AllocationFailed`. The
  infallible forms let the global allocator call `handle_alloc_error`, which
  aborts the process uncatchably — use the `try_*` forms for any size derived
  from untrusted input.

- **A serialized filter is not anonymous.** The hasher's SipHash key is part
  of the wire format, because queries need it. For blocked backends this is
  sharper than it sounds: the block is selected from the top
  `log2(num_blocks)` hash bits, so an attacker who knows the key needs only
  ~`num_blocks` trial hashes to steer items into a chosen block — and the
  resulting always-false-positive block is invisible to occupancy monitoring.

### Python bindings

- Every failure mode raises a normal Python exception. Oversized requests
  raise `ValueError`/`MemoryError` rather than aborting the interpreter, and
  out-of-range `hashes=` raises `ValueError` rather than
  `pyo3_runtime.PanicException` (which inherits `BaseException` and so escapes
  `except Exception:`).
- `str` and the equivalent `bytes` hash identically. A `str` holding a lone
  surrogate raises `UnicodeEncodeError`.
- `ClassicalFilter` and `BlockedFilter` are `unsendable`, matching the `!Sync`
  Rust types they wrap — sharing one across threads fails loudly.
  `AtomicFilter` is the shareable one, and its `insert_many` releases the GIL.
- Built against pyo3 0.29.

### Known limitations

- No `union` / `intersect` yet; `Filter::is_compatible` exists to guard them
  when they land.
- No deletion (this is a plain Bloom filter, not a counting one).
- `SimdBlocked` is slower than scalar `Blocked` on aarch64; the NEON path
  needs a redesign. See [`ROADMAP.md`](./ROADMAP.md).
- `Atomic` has a bench target but no published numbers — its cost is
  dominated by contention, which the single-threaded harness does not measure.
