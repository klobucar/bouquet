# Roadmap

Everything below is *deliberately deferred* — these items shape what the
`Backend` trait has to accommodate, but they are not promises for any
particular timeline.

## Shipped

- `Classical` — flat `Box<[Cell<u64>]>`.
- `Atomic` — `Box<[AtomicU64]>`, concurrent insert via `&self`.
- `Blocked` — 512-bit cache-line-sized blocks. Aimed at the
  insert-at-large-size gap; on Apple silicon the measured win is within
  noise, and it costs 1.2–14× the false-positive rate depending on target.
- `SimdBlocked` — `Blocked` with NEON / AVX2 hot-loop dispatch.

## Backends

### `AtomicBlocked` (next likely)

`Blocked` storage with `AtomicU64` cells, for concurrent inserts into a
blocked filter. Combines the cache-locality win of `Blocked` with the
threading model of `Atomic`. The hot path is a single `fetch_or` per word
across the 8 words of the block.

### NEON redesign for `SimdBlocked`

The NEON implementation used to run 2-2.5× slower than scalar `Blocked` on
Apple silicon (M3 Max). The cause was the mask build: it wrote the mask to
the stack one word at a time, and the vector loads that followed could not
be store-forwarded from those writes. Building the mask in registers fixed
that, and `SimdBlocked` now trails scalar `Blocked` by 1 to 3 ns per
operation instead.

It still does not win, because the scalar path touches only the words its
probes land in while the wide path loads and compares the whole block.
Approaches left to evaluate:

- Expand block size from 512 → 1024 bits (full Apple silicon cache line)
  so the wide load amortizes better. Trades a small FP-rate hit for
  bigger blocks.
- Vectorize the probe-derivation step itself (compute `k` indices in
  parallel as a vector). Plausible with NEON's variable-shift ops.

Until one of these wins the type is not the recommended pick on aarch64.
The AVX2 path shares the mask build but has not been benchmarked since the
fix.

### AVX-512 path for `SimdBlocked`

The current SIMD impl uses NEON on aarch64 and AVX2 on x86_64. AVX-512
would let one instruction set or test all 8 lanes of a 512-bit block, but
the frequency-throttling story on Intel client CPUs is messy. Probably
gated behind a separate `simd-avx512` feature so callers opt in.

### SVE path for `SimdBlocked`

Apple M4 and ARM server chips (Graviton 3+, Neoverse V1+) have SVE.
Variable-vector-width support means one binary can target multiple
hardware widths. Worth doing once SVE intrinsics stabilise on stable Rust.

## Filter Variants

Out of scope for any 0.x release; tracked here so they aren't reinvented:

- **Counting Bloom**: cells become small counters instead of bits; supports
  removal at the cost of ~4× memory.
- **Cuckoo filter**: different data structure entirely; better FP-per-byte at
  load factors below ~70%, supports deletion.
- **Quotient filter**: cache-friendly alternative with fingerprint storage.
- **Scalable / growable Bloom**: chain of filters that fills as the dataset
  grows. Useful when the item count is unknown up front.

## Operational

- **Compressed serialization**: the wire format is currently uncompressed.
  Sparse filters compress well; lz4 / zstd would be a 1-line addition behind
  a feature.
- **mmap persistence**: a `MmapBacked` backend that reads bits from a
  read-only mapping. Useful for large filters where the cost of loading from
  disk is amortized over many processes.
- ~~**`union` / `intersect`**~~: done. `Filter::union_with` and
  `Filter::intersect_with` combine filters that share
  `(num_bits, k, hasher)` and return `Error::Incompatible` otherwise. Not
  yet exposed in the Python bindings.
- ~~**Builder errors instead of panics**~~: done.
  `WithFalsePos::try_expected_items`, `WithNumBits::try_expected_items`, and
  `WithNumBits::try_hashes` return `Result` and report allocation failure
  instead of aborting. The infallible forms remain for literal sizes.
