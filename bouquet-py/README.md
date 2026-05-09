# bouquet (Python bindings)

Python bindings for [`bouquet`](../crates/bouquet) — pluggable Bloom filters
with a Rust core.

```python
import bouquet

f = bouquet.ClassicalFilter.from_false_pos(0.001, 100_000, seed=0xfeedface)
f.insert(b"hello")
assert b"hello" in f

# Concurrent inserts: GIL is released during the bulk loop.
g = bouquet.AtomicFilter.from_false_pos(0.001, 1_000_000)
items = [str(i).encode() for i in range(1_000_000)]
duplicates = g.insert_many(items)  # parallelizable across Python threads
```

## Installation

Wheels are published to PyPI per release. Until the first release, build
from source:

```sh
# from the workspace root
cd bouquet-py
uv venv
source .venv/bin/activate
uv pip install maturin pytest
maturin develop --release    # builds the Rust extension and installs it
pytest                       # runs the smoke tests
```

`maturin develop` rebuilds the extension on every Python source / Rust
source change. For wheel builds (CI):

```sh
maturin build --release
ls target/wheels/            # bouquet-*.whl
```

## What's exposed

| Python class      | Rust backend         | Notes                                 |
|-------------------|----------------------|---------------------------------------|
| `ClassicalFilter` | `bouquet::Classical` | Single-threaded inserts.              |
| `AtomicFilter`    | `bouquet::Atomic`    | Concurrent inserts; releases the GIL. |
| `BlockedFilter`   | `bouquet::Blocked`   | Cache-line-blocked layout.            |

`SimdBlocked` is intentionally not exposed yet — see
[`ROADMAP.md`](../ROADMAP.md) at the workspace root for the open redesign.

## Hashing model

Methods take `bytes`-like input (`bytes`, `bytearray`, `memoryview`). For
arbitrary Python objects, encode them yourself before calling — `str.encode()`
for strings, an explicit serializer for richer types. This keeps the hash
domain explicit instead of hidden behind `__hash__`.

## License

MIT — see [`LICENSE`](../LICENSE) at the workspace root.
