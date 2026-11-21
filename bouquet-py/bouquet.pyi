# Type stubs for the `bouquet` extension module (compiled from Rust).
#
# Hand-maintained to mirror `src/lib.rs`; keep in sync when the `#[pymethods]`
# surface changes. All single-item methods accept any bytes-like object or a
# `str` (UTF-8 encoded before hashing), so `f.insert("x")` and `f.insert(b"x")`
# produce identical filter state.
#
# Note the API is deliberately asymmetric:
#   - `from_num_bits` exists only on `ClassicalFilter`.
#   - `insert_many` (which releases the GIL) exists only on `AtomicFilter`.
#   - `ClassicalFilter` and `BlockedFilter` are pinned to the thread that
#     created them; sharing one across threads raises. Use `AtomicFilter`
#     for concurrent access.
#
# None of these classes can be constructed directly or subclassed — use the
# `from_*` static methods.

from collections.abc import Sequence
from typing import TypeAlias

__version__: str

# What the filters accept for a single item. At runtime *any* buffer-protocol
# object is accepted (`array.array("B", ...)`, strided memoryviews, ...), but
# `collections.abc.Buffer` is 3.12+ and this package supports 3.11, so the
# alias names the concrete types instead. It is narrower than reality, never
# wider.
_Item: TypeAlias = bytes | bytearray | memoryview | str

class ClassicalFilter:
    @staticmethod
    def from_false_pos(
        false_pos: float,
        expected_items: int,
        *,
        seed: int | None = ...,
    ) -> ClassicalFilter: ...
    @staticmethod
    def from_num_bits(
        num_bits: int,
        *,
        expected_items: int | None = ...,
        hashes: int | None = ...,
        seed: int | None = ...,
    ) -> ClassicalFilter: ...
    def insert(self, item: _Item) -> bool: ...
    def contains(self, item: _Item) -> bool: ...
    def __contains__(self, item: _Item) -> bool: ...
    def clear(self) -> None: ...
    def estimate_count(self) -> int: ...
    def is_saturated(self) -> bool: ...
    @property
    def num_bits(self) -> int: ...
    @property
    def num_hashes(self) -> int: ...

class AtomicFilter:
    @staticmethod
    def from_false_pos(
        false_pos: float,
        expected_items: int,
        *,
        seed: int | None = ...,
    ) -> AtomicFilter: ...
    def insert(self, item: _Item) -> bool: ...
    def contains(self, item: _Item) -> bool: ...
    def __contains__(self, item: _Item) -> bool: ...
    # A real sequence (list, tuple) — not any iterable. Extraction happens
    # on the Rust side via PySequence, so generators raise TypeError. A bare
    # `str`/`bytes` is rejected rather than iterated element-wise, even
    # though `str` is itself a Sequence[str].
    def insert_many(self, items: Sequence[_Item]) -> int: ...
    def clear(self) -> None: ...
    def estimate_count(self) -> int: ...
    def is_saturated(self) -> bool: ...
    @property
    def num_bits(self) -> int: ...
    @property
    def num_hashes(self) -> int: ...

class BlockedFilter:
    @staticmethod
    def from_false_pos(
        false_pos: float,
        expected_items: int,
        *,
        seed: int | None = ...,
    ) -> BlockedFilter: ...
    def insert(self, item: _Item) -> bool: ...
    def contains(self, item: _Item) -> bool: ...
    def __contains__(self, item: _Item) -> bool: ...
    def clear(self) -> None: ...
    def estimate_count(self) -> int: ...
    def is_saturated(self) -> bool: ...
    @property
    def num_bits(self) -> int: ...
    @property
    def num_hashes(self) -> int: ...
