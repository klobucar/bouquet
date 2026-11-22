# bouquet-bloom (Python bindings)

Python bindings for [`bouquet`](https://github.com/klobucar/bouquet) —
pluggable Bloom filters with a Rust core.

Install [`bouquet-bloom`](https://pypi.org/project/bouquet-bloom/) for
CPython 3.11 or later, then import it as `bouquet`.

```sh
python -m pip install bouquet-bloom
```

```python
import bouquet

f = bouquet.ClassicalFilter.from_false_pos(0.001, 100_000, seed=0xFEEDFACE)
f.insert("hello")  # str is fine — encoded as UTF-8 before hashing
assert "hello" in f
assert b"hello" in f  # bytes work too; str + equivalent bytes hash the same

# Concurrent inserts: GIL is released during the bulk loop.
g = bouquet.AtomicFilter.from_false_pos(0.001, 1_000_000)
items = [str(i) for i in range(1_000_000)]
duplicates = g.insert_many(items)  # parallelizable across Python threads
```

## Building from source

Any Python 3.11+ works; one ABI3 wheel serves all of them.

```sh
cd bouquet-py
uv sync                      # locked dev environment + builds the cdylib
uv run --no-sync pytest      # runs the test suite
```

`--no-sync` matters after a Rust change: a bare `uv run` re-syncs the project
and can reinstall a cached wheel over the extension you just built. The
reliable loop is:

```sh
uv run --no-sync maturin develop --release
uv run --no-sync pytest
```

`uv sync` invokes maturin transparently because of the `build-system =
{ build-backend = "maturin" }` section in `pyproject.toml`. After a Rust
source change, force a rebuild + reinstall:

```sh
uv sync --reinstall-package bouquet-bloom
```

For wheel builds (CI uses this):

```sh
uv run maturin build --release
ls ../target/wheels/         # bouquet_bloom-*.whl, one ABI3 wheel per OS/arch
```

## What's exposed

| Python class      | Rust backend         | Notes                                 |
|-------------------|----------------------|---------------------------------------|
| `ClassicalFilter` | `bouquet::Classical` | Single-threaded. The general-purpose default. |
| `AtomicFilter`    | `bouquet::Atomic`    | Concurrent; `insert_many` and `contains_many` release the GIL. |
| `BlockedFilter`   | `bouquet::Blocked`   | Cache-line-blocked layout. Trades accuracy for locality — see the root README. |

Every class has the same methods: the `from_false_pos` and `from_num_bits`
constructors, `insert`, `contains`, `__contains__`, `insert_many`,
`contains_many`, `clear`, `estimate_count`, `is_saturated`, `to_bytes`,
`from_bytes`, `num_bits`, and `num_hashes`.

## Batch calls

`insert_many` and `contains_many` take a list or tuple and make one call
into Rust instead of one per item. On 100,000 short strings that measured
about 30% faster than a Python loop over `insert` or `contains`.

```python
f = bouquet.ClassicalFilter.from_false_pos(0.001, 100_000)
duplicates = f.insert_many(["a", "b", "c"])  # count already present
hits = f.contains_many(["a", "z"])  # [True, False]
```

On `AtomicFilter` both release the GIL while they run. They take a real
sequence, not a generator, and reject a bare `str` or `bytes` instead of
iterating it character by character.

## Saving and loading

```python
blob = f.to_bytes()
g = bouquet.ClassicalFilter.from_bytes(blob)
assert "a" in g
```

The bytes include the filter's hash key, because the key is needed to query
it after loading. Anyone who can read them can test membership and craft
colliding inputs, so store them as carefully as the data the filter
describes, and only load bytes you trust.

`ClassicalFilter` and `AtomicFilter` share a bit layout and can load each
other's bytes. `BlockedFilter` uses a different layout; loading across that
boundary raises `ValueError` rather than returning a filter that answers
wrongly. The format carries a version byte, and a future release that
changes it will reject old bytes instead of misreading them.

`ClassicalFilter` and `BlockedFilter` wrap `!Sync` Rust types and are pinned
to the thread that created them — passing one to another thread raises rather
than racing. `AtomicFilter` is the one built for sharing.

`SimdBlocked` is intentionally not exposed yet — see
[ROADMAP.md](https://github.com/klobucar/bouquet/blob/main/ROADMAP.md) for
the open redesign.

## Hashing model

Methods accept `bytes` / `bytearray` / `memoryview` or `str`. Strings are
encoded as UTF-8 before hashing, so `f.insert("hello")` and `f.insert(b"hello")`
produce identical filter state — pass whichever is more natural at the call
site.

For richer Python objects, serialize to bytes yourself before calling. This
keeps the hash domain explicit instead of hidden behind `__hash__`, and
avoids the cross-process instability that `PYTHONHASHSEED` randomization
would otherwise introduce.

## License

MIT — see [LICENSE](https://github.com/klobucar/bouquet/blob/main/LICENSE).
