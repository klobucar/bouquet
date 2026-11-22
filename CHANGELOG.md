# Changelog

All notable changes to this project are documented here. This project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html); pre-1.0, minor
versions may break.

## [Unreleased]

### Added

- `Filter::union_with` and `Filter::intersect_with`, for filters that share
  a size, hash count, and hasher. Anything else returns the new
  `Error::Incompatible` and leaves the filter untouched.
- `Filter::insert_all`, which inserts every item of an iterator and returns
  the number that were probably already present, and an `Extend`
  implementation for `Filter`.
- Python: every class now has `from_num_bits`, `insert_many`,
  `contains_many`, `to_bytes`, and `from_bytes`. Previously `from_num_bits`
  was `ClassicalFilter`-only, `insert_many` was `AtomicFilter`-only, and
  there was no way to save a filter. The batch calls measured about 30%
  faster than a Python loop on 100,000 short strings; on `AtomicFilter`
  they release the GIL.
- A `fill` benchmark group that times building a filter from empty, and a
  matching README table.

### Changed

- **Breaking for custom backends:** the `Backend` trait has two new
  required methods, `union_with` and `intersect_with`. Code that only uses
  the built-in backends is unaffected.
- Insert and lookup hot paths no longer branch on filter contents. Filling
  an empty 1M-item `Classical` filter went from about 37 to about 11 ns per
  item, and absent lookups on `Classical`, `Atomic`, and `Blocked` from
  about 21 to about 10 ns (M3 Max). Re-inserting items already present
  into a filter much larger than the cache may be slightly slower, because
  the store is now unconditional.
- `SimdBlocked` builds its block mask in registers, removing a
  store-forwarding stall. It went from about 22 to about 12 ns per
  operation on aarch64 and now trails scalar `Blocked` by 1 to 3 ns rather
  than 2×. The 0.1.0 "known limitation" about it is out of date to that
  extent.
- Python: `str` arguments are dispatched before `bytes`, which took
  `insert(str)` from about 54 to about 40 ns, and `insert_many` borrows
  `str` and `bytes` items instead of copying them.
- The Python wheel now depends on `postcard` (already a dev-dependency of
  the core crate) for `to_bytes` / `from_bytes`.
- README links are absolute, so the banner and links render on crates.io.

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
- **The Python package (`bouquet-bloom`) now shares the crate's version.**
  Both inherit one `[workspace.package] version`, so a wheel always carries
  the version of the core crate it was built from. Until now they were
  versioned independently, so `bouquet-bloom` 0.1.0 (tag `py-v0.1.0`) was
  published from the same commit as crate 0.1.1 (tag `v0.1.1`). The
  `bouquet-bloom` 0.1.1 wheel has the same bindings code, rebuilt with the
  fixes above.

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
