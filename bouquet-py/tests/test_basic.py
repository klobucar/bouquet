"""Smoke tests for the bouquet Python bindings.

Run after `maturin develop` to install the extension into the active
virtualenv. CI runs `maturin build` and then `pip install dist/*.whl`
followed by `pytest`.
"""

from __future__ import annotations

import pytest

import bouquet


def test_module_has_expected_classes():
    assert hasattr(bouquet, "ClassicalFilter")
    assert hasattr(bouquet, "AtomicFilter")
    assert hasattr(bouquet, "BlockedFilter")
    assert isinstance(bouquet.__version__, str)


@pytest.mark.parametrize(
    "cls",
    [
        bouquet.ClassicalFilter,
        bouquet.AtomicFilter,
        bouquet.BlockedFilter,
    ],
)
def test_round_trip_membership(cls):
    f = cls.from_false_pos(0.01, 1_000, seed=0xDEADBEEF)
    f.insert(b"hello")
    f.insert(b"world")
    assert f.contains(b"hello")
    assert f.contains(b"world")
    assert b"hello" in f
    assert b"world" in f
    assert not f.contains(b"never-inserted")


@pytest.mark.parametrize(
    "cls",
    [
        bouquet.ClassicalFilter,
        bouquet.AtomicFilter,
        bouquet.BlockedFilter,
    ],
)
def test_clear_resets(cls):
    f = cls.from_false_pos(0.01, 1_000, seed=1)
    f.insert(b"a")
    assert f.contains(b"a")
    f.clear()
    assert not f.contains(b"a")


def test_classical_explicit_num_bits():
    f = bouquet.ClassicalFilter.from_num_bits(1024, hashes=4, seed=0)
    assert f.num_bits == 1024
    assert f.num_hashes == 4


def test_classical_rejects_bad_args():
    with pytest.raises(ValueError):
        bouquet.ClassicalFilter.from_false_pos(0.0, 100)
    with pytest.raises(ValueError):
        bouquet.ClassicalFilter.from_false_pos(1.0, 100)
    with pytest.raises(ValueError):
        bouquet.ClassicalFilter.from_false_pos(0.01, 0)
    with pytest.raises(ValueError):
        bouquet.ClassicalFilter.from_num_bits(0, hashes=4)
    with pytest.raises(ValueError):
        # both expected_items and hashes — must pass exactly one
        bouquet.ClassicalFilter.from_num_bits(1024, expected_items=100, hashes=4)
    with pytest.raises(ValueError):
        # neither
        bouquet.ClassicalFilter.from_num_bits(1024)


def test_atomic_insert_many_releases_gil():
    """Smoke test: insert_many works and returns the duplicate count.

    The GIL release isn't actually verifiable from a single-threaded test —
    the property holds because of how `py.allow_threads` is invoked. CI
    correctness is asserted by the round-trip test.
    """
    f = bouquet.AtomicFilter.from_false_pos(0.001, 10_000, seed=42)
    items = [f"item-{i}".encode() for i in range(1000)]
    dups_first = f.insert_many(items)
    assert dups_first == 0
    dups_second = f.insert_many(items)
    # Re-inserting the same items should report most as duplicates. Not all,
    # because the FP-rate-driven first insert may have set bits that make
    # later items spuriously appear "fully present". With FP=0.001 and 1000
    # items, the floor is comfortably > 900.
    assert dups_second > 900


@pytest.mark.parametrize(
    "cls",
    [
        bouquet.ClassicalFilter,
        bouquet.AtomicFilter,
        bouquet.BlockedFilter,
    ],
)
def test_str_input_accepted(cls):
    """`str` inputs are encoded as UTF-8 and hash identically to the
    equivalent `bytes`. This is the friendlier API: callers can pass
    cache keys / URLs / IDs directly without `.encode()`."""
    f = cls.from_false_pos(0.01, 1_000, seed=0)
    f.insert("hello")
    assert f.contains("hello")
    assert "hello" in f
    # str and bytes equivalence — same hash, same filter state.
    assert f.contains(b"hello")
    assert b"hello" in f
    # Non-ASCII round-trips through UTF-8.
    f.insert("café")
    assert f.contains("café")
    assert f.contains("café".encode())


@pytest.mark.parametrize(
    "cls",
    [
        bouquet.ClassicalFilter,
        bouquet.AtomicFilter,
        bouquet.BlockedFilter,
    ],
)
def test_bytearray_and_memoryview_still_work(cls):
    """Regression guard: switching to PyBackedBytes / PyBackedStr / Vec<u8>
    dispatch must not drop the bytes-like types the original `&[u8]`
    signature accepted."""
    f = cls.from_false_pos(0.01, 1_000, seed=0)
    f.insert(bytearray(b"ba"))
    f.insert(memoryview(b"mv"))
    assert f.contains(b"ba")
    assert f.contains(b"mv")
    assert f.contains(bytearray(b"ba"))
    assert f.contains(memoryview(b"mv"))


def test_atomic_insert_many_accepts_mixed_str_and_bytes():
    """`insert_many` should accept a list mixing `str` and `bytes`. The
    OwnedBytesOrStr dispatch handles each item independently."""
    f = bouquet.AtomicFilter.from_false_pos(0.001, 1_000, seed=0)
    items = ["alpha", b"beta", "gamma", bytearray(b"delta")]
    dups = f.insert_many(items)
    assert dups == 0
    for item in ("alpha", "beta", "gamma", "delta"):
        assert f.contains(item)
        assert f.contains(item.encode())


def test_rejects_unsupported_types():
    """Non-bytes/str inputs should raise a clear error rather than coerce."""
    f = bouquet.ClassicalFilter.from_false_pos(0.01, 100, seed=0)
    with pytest.raises((TypeError, ValueError)):
        f.insert(42)
    with pytest.raises((TypeError, ValueError)):
        f.insert(None)


def test_repr_is_useful():
    f = bouquet.ClassicalFilter.from_false_pos(0.01, 1_000, seed=0)
    r = repr(f)
    assert "ClassicalFilter" in r
    assert "num_bits" in r
    assert "num_hashes" in r


def test_seed_determinism():
    """Two filters built with the same seed must agree on every membership
    answer; two with different seeds must diverge on at least some inputs.

    Filter is loaded to its design capacity so the FP rate is the configured
    1%. With ~40 FPs expected per filter across 4000 queries and the FPs
    falling on different items between the two seed values, the divergence
    check is robust — if you under-load this filter the test goes flaky
    because rare FPs may happen to coincide.
    """
    a = bouquet.ClassicalFilter.from_false_pos(0.01, 1_000, seed=0xAAAA)
    b = bouquet.ClassicalFilter.from_false_pos(0.01, 1_000, seed=0xAAAA)
    c = bouquet.ClassicalFilter.from_false_pos(0.01, 1_000, seed=0xBBBB)

    for i in range(1_000):
        a.insert(str(i).encode())
        b.insert(str(i).encode())
        c.insert(str(i).encode())

    diverged = False
    for i in range(10_000, 14_000):
        x = str(i).encode()
        assert a.contains(x) == b.contains(x), f"same-seed diverged at {i}"
        if a.contains(x) != c.contains(x):
            diverged = True
    assert diverged, "different-seed filters never disagreed on a non-inserted query"
