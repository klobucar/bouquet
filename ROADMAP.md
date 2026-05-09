# Roadmap

Everything below is *deliberately deferred* — these items shape what the
`Backend` trait has to accommodate, but they are not promises for any
particular timeline.

## Shipped

- `Classical` (v0.1) — flat `Box<[Cell<u64>]>`.
- `Atomic` (v0.1) — `Box<[AtomicU64]>`, concurrent insert via `&self`.
- `Blocked` (v0.2) — 512-bit cache-line-sized blocks. Closes the
  insert-at-large-size gap.
- `SimdBlocked` (v0.2) — `Blocked` with NEON / AVX2 hot-loop dispatch.

## Backends

### `AtomicBlocked` (next likely)

`Blocked` storage with `AtomicU64` cells, for concurrent inserts into a
blocked filter. Combines the cache-locality win of `Blocked` with the
threading model of `Atomic`. The hot path is a single `fetch_or` per word
across the 8 words of the block.

### NEON redesign for `SimdBlocked`

The current NEON implementation is *correct* but *slower than scalar
`Blocked`* on Apple silicon (M3 Max measured), by ~2-2.5× at
`contains_present`. The mask-build-then-load pattern hits store-to-load
forwarding stalls, and the 4× 128-bit register layout doesn't amortize the
SIMD setup overhead for a block that fits in one cache line.

Approaches to evaluate:

- Build the OR-mask directly in NEON registers using `vsetq_lane_u64`,
  avoiding the stack round-trip. Branchy on the lane index but fits k=7.
- Expand block size from 512 → 1024 bits (full Apple silicon cache line)
  so the wide load amortizes better. Trades a small FP-rate hit for
  bigger blocks.
- Vectorize the probe-derivation step itself (compute `k` indices in
  parallel as a vector). Plausible with NEON's variable-shift ops.

Until one of these wins on `contains_present` the type is honest about
its current state but not the recommended pick on aarch64.

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
- **`union` / `intersect`**: cheap when filters share `(num_bits, k, hasher)`.
  The `Filter::is_compatible` predicate already exists; the operations
  themselves are a small followup.
- **Builder errors instead of panics**: the v0.1 builder panics on invalid
  inputs (`fp <= 0`, `num_bits == 0`, etc.). A `try_*` variant returning
  `Result` would be friendly for callers that take untrusted parameters.
