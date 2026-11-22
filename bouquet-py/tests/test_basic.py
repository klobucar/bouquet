"""Smoke tests for the bouquet Python bindings.

Run after `maturin develop` to install the extension into the active
virtualenv. CI runs `maturin build` and then `pip install dist/*.whl`
followed by `pytest`.
"""

from __future__ import annotations

import threading

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
        f.insert(42)  # ty: ignore[invalid-argument-type]  # deliberately wrong type
    with pytest.raises((TypeError, ValueError)):
        f.insert(None)  # ty: ignore[invalid-argument-type]  # deliberately wrong type
    # Sequences of ints must NOT silently coerce to bytes: [104, 105] is not
    # b"hi". (A derived Vec<u8> fallback used to accept these.)
    with pytest.raises(TypeError):
        f.insert([104, 105])  # ty: ignore[invalid-argument-type]  # deliberately wrong type
    with pytest.raises(TypeError):
        f.insert((104, 105))  # ty: ignore[invalid-argument-type]  # deliberately wrong type


def test_insert_many_rejects_int_sequences():
    """The same no-coercion rule holds per item inside insert_many."""
    f = bouquet.AtomicFilter.from_false_pos(0.01, 100, seed=0)
    with pytest.raises(TypeError):
        f.insert_many([[104, 105]])  # ty: ignore[invalid-argument-type]


def test_seed_accepts_wide_ints():
    """Seeds are full 128-bit values, matching the Rust API: a seed above
    2**64 must be accepted, not OverflowError, and must be deterministic."""
    big = (1 << 100) | 12345
    a = bouquet.ClassicalFilter.from_false_pos(0.01, 1_000, seed=big)
    b = bouquet.ClassicalFilter.from_false_pos(0.01, 1_000, seed=big)
    for i in range(100):
        a.insert(str(i))
        b.insert(str(i))
    for i in range(5_000, 6_000):
        assert a.contains(str(i)) == b.contains(str(i))
    with pytest.raises(OverflowError):
        bouquet.ClassicalFilter.from_false_pos(0.01, 100, seed=1 << 128)


def test_rejects_out_of_range_hashes():
    """`hashes` used to reach a Rust `panic!`, surfacing as PanicException —
    which inherits BaseException and so escapes `except Exception`."""
    for k in (0, 1025, 2000, 2**31):
        with pytest.raises(ValueError):
            bouquet.ClassicalFilter.from_num_bits(1024, hashes=k)

    # Specifically: catchable as Exception, not just BaseException — a
    # PanicException would slip straight through this.
    with pytest.raises(Exception):  # noqa: B017
        bouquet.ClassicalFilter.from_num_bits(1024, hashes=0)


@pytest.mark.parametrize(
    "call",
    [
        lambda: bouquet.ClassicalFilter.from_num_bits(10**15, hashes=4),
        lambda: bouquet.ClassicalFilter.from_false_pos(0.001, 10**13),
        lambda: bouquet.AtomicFilter.from_false_pos(0.001, 10**13),
        lambda: bouquet.BlockedFilter.from_false_pos(0.001, 10**13),
        lambda: bouquet.ClassicalFilter.from_false_pos(1e-300, 10**18),
        lambda: bouquet.ClassicalFilter.from_num_bits(2**64 - 1, hashes=4),
    ],
)
def test_oversized_requests_raise_instead_of_killing_the_interpreter(call):
    """These used to abort the process (SIGABRT) or SIGKILL it via the OOM
    killer, with no traceback and nothing catchable. All three backends now
    agree on refusing, and they refuse with a normal Python exception."""
    with pytest.raises((ValueError, MemoryError, OverflowError)):
        call()


def test_surrogate_str_raises_unicode_error_not_a_confusing_type_error():
    """A lone surrogate is a `str`, so 'expected str ..., got str' was both
    wrong and unactionable. Lone surrogates arrive routinely from
    os.fsdecode and surrogateescape-decoded filenames."""
    f = bouquet.ClassicalFilter.from_false_pos(0.01, 100, seed=0)
    with pytest.raises(UnicodeEncodeError):
        f.insert("\ud800")
    with pytest.raises(UnicodeEncodeError):
        f.contains("\ud800")
    with pytest.raises(UnicodeEncodeError):
        "\ud800" in f  # noqa: B015


def test_insert_many_rejects_a_bare_bytes_or_str():
    """`insert_many(b"abc")` iterated the bytes as ints and complained that a
    bytes-like object wasn't bytes-like."""
    f = bouquet.AtomicFilter.from_false_pos(0.01, 100, seed=0)
    for bad in (b"abc", bytearray(b"abc"), "abc"):
        with pytest.raises(TypeError, match="sequence of items"):
            f.insert_many(bad)  # ty: ignore[invalid-argument-type]

    # The correct form still works.
    assert f.insert_many([b"abc"]) == 0


def test_estimate_count_saturation_is_flagged():
    f = bouquet.ClassicalFilter.from_num_bits(64, hashes=1, seed=0)
    assert not f.is_saturated()
    for i in range(20_000):
        f.insert(str(i))
    assert f.is_saturated()
    # The sentinel is documented rather than silently plausible.
    assert f.estimate_count() == 2**64 - 1


def test_blocked_reports_the_k_it_actually_uses():
    """Blocked backends clamp k to the block budget; num_hashes must report
    the clamped value rather than the request."""
    f = bouquet.BlockedFilter.from_false_pos(1e-160, 1000, seed=1)
    assert f.num_hashes <= 64
    for i in range(1000):
        f.insert(str(i))
    hits = sum(1 for i in range(10_000, 20_000) if f.contains(str(i)))
    # Unclamped, this configuration answered True for ~half of all queries.
    assert hits / 10_000 < 0.01


def test_single_threaded_filters_are_pinned_to_their_thread():
    """`ClassicalFilter`/`BlockedFilter` wrap a `!Sync` backend. Sharing one
    across threads must fail loudly rather than race."""
    f = bouquet.ClassicalFilter.from_false_pos(0.01, 100, seed=0)
    f.insert(b"x")
    errors = []

    def worker():
        try:
            f.insert(b"y")
        except BaseException as e:  # noqa: BLE001
            errors.append(e)

    t = threading.Thread(target=worker)
    t.start()
    t.join()
    assert errors, "expected cross-thread access to be refused"


def test_atomic_filter_is_shareable_across_threads():
    """...and the concurrent backend is the one that is not pinned."""
    f = bouquet.AtomicFilter.from_false_pos(0.01, 10_000, seed=0)

    def worker(lo):
        for i in range(lo, lo + 1000):
            f.insert(str(i))

    threads = [threading.Thread(target=worker, args=(n * 1000,)) for n in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    for i in range(4000):
        assert f.contains(str(i)), f"lost {i}"


def test_insert_many_releases_the_gil():
    """The headline claim of the binding, made checkable: a spinner thread
    must make real progress while insert_many runs."""
    f = bouquet.AtomicFilter.from_false_pos(0.01, 2_000_000, seed=0)
    items = [str(i).encode() for i in range(1_000_000)]

    ticks = 0
    stop = False

    def spinner():
        nonlocal ticks
        while not stop:
            ticks += 1

    t = threading.Thread(target=spinner)
    t.start()
    try:
        f.insert_many(items)
    finally:
        stop = True
        t.join()

    assert ticks > 1000, f"spinner only advanced {ticks} times; GIL likely held"


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


ALL_CLASSES = [bouquet.ClassicalFilter, bouquet.AtomicFilter, bouquet.BlockedFilter]


@pytest.mark.parametrize("cls", ALL_CLASSES)
def test_from_num_bits_on_every_class(cls):
    f = cls.from_num_bits(1 << 16, hashes=4, seed=1)
    assert f.num_bits == 1 << 16
    assert f.num_hashes == 4
    g = cls.from_num_bits(1 << 16, expected_items=5_000)
    assert g.num_hashes >= 1
    with pytest.raises(ValueError):
        cls.from_num_bits(1 << 16)
    with pytest.raises(ValueError):
        cls.from_num_bits(1 << 16, expected_items=10, hashes=4)
    with pytest.raises(ValueError):
        cls.from_num_bits(0, hashes=4)


@pytest.mark.parametrize("cls", ALL_CLASSES)
def test_batch_methods_match_single_item_methods(cls):
    items = [f"item-{i}" for i in range(500)]
    mixed = [s if i % 2 else s.encode() for i, s in enumerate(items)]

    one_by_one = cls.from_false_pos(0.001, 10_000, seed=7)
    for item in items:
        one_by_one.insert(item)
    batched = cls.from_false_pos(0.001, 10_000, seed=7)
    assert batched.insert_many(mixed) == 0
    assert batched.to_bytes() == one_by_one.to_bytes()

    assert batched.insert_many(tuple(items)) > 450
    queries = [*items[:3], "absent-1", b"absent-2"]
    assert batched.contains_many(queries) == [batched.contains(q) for q in queries]
    assert batched.contains_many(queries)[:3] == [True, True, True]
    assert batched.contains_many([]) == []
    assert batched.insert_many([]) == 0


@pytest.mark.parametrize("cls", ALL_CLASSES)
@pytest.mark.parametrize("method", ["insert_many", "contains_many"])
def test_batch_methods_reject_bad_arguments(cls, method):
    call = getattr(cls.from_false_pos(0.01, 100, seed=0), method)
    for bad in (b"abc", bytearray(b"abc"), "abc"):
        with pytest.raises(TypeError, match="sequence of items"):
            call(bad)
    with pytest.raises(TypeError):
        call([1, 2, 3])
    with pytest.raises(TypeError):
        call(x for x in ["a"])


@pytest.mark.parametrize("cls", ALL_CLASSES)
def test_bytes_round_trip(cls):
    f = cls.from_false_pos(0.001, 5_000)  # random key: it must travel too
    items = [f"item-{i}" for i in range(1_000)]
    f.insert_many(items)

    blob = f.to_bytes()
    assert isinstance(blob, bytes)
    g = cls.from_bytes(blob)

    assert (g.num_bits, g.num_hashes) == (f.num_bits, f.num_hashes)
    assert all(g.contains_many(items))
    assert g.estimate_count() == f.estimate_count()
    assert g.to_bytes() == blob
    # The copy is independent of the original.
    g.insert("only-in-g")
    assert g.to_bytes() != f.to_bytes()


def test_flat_layouts_interchange_but_blocked_does_not():
    classical = bouquet.ClassicalFilter.from_false_pos(0.01, 1_000, seed=3)
    classical.insert("x")
    assert "x" in bouquet.AtomicFilter.from_bytes(classical.to_bytes())

    with pytest.raises(ValueError, match="layout"):
        bouquet.BlockedFilter.from_bytes(classical.to_bytes())
    blocked = bouquet.BlockedFilter.from_false_pos(0.01, 1_000, seed=3)
    with pytest.raises(ValueError, match="layout"):
        bouquet.ClassicalFilter.from_bytes(blocked.to_bytes())


@pytest.mark.parametrize("cls", ALL_CLASSES)
def test_from_bytes_rejects_garbage(cls):
    blob = cls.from_false_pos(0.01, 1_000, seed=3).to_bytes()
    bad_blobs = [
        b"",
        b"not a filter",
        blob[:4],  # header only
        blob[: len(blob) // 2],  # truncated
        blob[:3] + b"\x02" + blob[4:],  # unknown format version
        b"\x00" * len(blob),
    ]
    for bad in bad_blobs:
        with pytest.raises(ValueError):
            cls.from_bytes(bad)
    with pytest.raises(TypeError):
        cls.from_bytes("a str")
