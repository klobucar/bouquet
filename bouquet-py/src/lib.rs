//! Python bindings for [`bouquet`](https://crates.io/crates/bouquet).
//!
//! Each backend exposed to Python is its own `#[pyclass]` because Rust
//! generics don't cross the FFI. Methods accept `bytes` / `bytearray` /
//! `memoryview` or `str` — strings are encoded as UTF-8 before hashing, so
//! `f.insert("hello")` and `f.insert(b"hello")` produce identical filter
//! state. For richer Python objects, callers should serialize to bytes
//! themselves so the hash domain stays explicit.

// PyO3's `#[staticmethod]` macro expansion generates `.into()` calls that
// clippy reports as `useless_conversion`. Allowed crate-wide because the
// offending code is in expanded macros, not source we control.
#![allow(clippy::useless_conversion)]

use ::bouquet::{Atomic, Blocked, Classical, Filter};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::pybacked::{PyBackedBytes, PyBackedStr};

// ---------------------------------------------------------------------------
// Input dispatch: accept bytes-like or str
// ---------------------------------------------------------------------------

/// Input for the single-item methods. PyO3 tries variants in order:
/// `Bytes` covers `bytes` and `bytearray` (zero-copy — `PyBackedBytes`
/// keeps a Python reference); `Str` covers `str` (CPython's UTF-8 cache,
/// zero-copy when already encoded); `Other` is the buffer-protocol
/// fallback for `memoryview` and the like, which pays one allocation
/// since arbitrary buffer objects don't give us a stable borrow. All
/// arms hash the same underlying bytes, so `f.insert("hello")` and
/// `f.insert(b"hello")` produce identical filter state.
#[derive(FromPyObject)]
enum BytesOrStr {
    Bytes(PyBackedBytes),
    Str(PyBackedStr),
    Other(Vec<u8>),
}

impl BytesOrStr {
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(b) => b,
            Self::Str(s) => s.as_bytes(),
            Self::Other(v) => v,
        }
    }
}

/// Owned variant used by `AtomicFilter.insert_many`, where the GIL is
/// released for the duration of the loop. Items are extracted to owned
/// `Vec<u8>` / `String` up front so the inserts can run without holding a
/// Python reference across the GIL boundary.
#[derive(FromPyObject)]
enum OwnedBytesOrStr {
    Bytes(Vec<u8>),
    Str(String),
}

impl OwnedBytesOrStr {
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(b) => b,
            Self::Str(s) => s.as_bytes(),
        }
    }
}

/// Validate a target false-positive rate before passing it to the bouquet
/// builder, since `bouquet`'s builder panics on invalid inputs and we want a
/// typed Python `ValueError` instead.
fn check_fp(fp: f64) -> PyResult<()> {
    if !fp.is_finite() || fp <= 0.0 || fp >= 1.0 {
        return Err(PyValueError::new_err(format!(
            "false_pos must be in (0, 1), got {fp}"
        )));
    }
    Ok(())
}

fn check_items(n: usize) -> PyResult<()> {
    if n == 0 {
        return Err(PyValueError::new_err("expected_items must be > 0"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ClassicalFilter
// ---------------------------------------------------------------------------

/// Single-threaded Bloom filter with a flat textbook layout.
///
/// Construct with `ClassicalFilter.from_false_pos(fp, expected_items)` (or
/// `.from_num_bits(...)`). Methods accept `bytes` / `bytearray` /
/// `memoryview` or `str` (UTF-8 encoded).
#[pyclass(name = "ClassicalFilter")]
struct ClassicalFilter {
    inner: Filter<Classical>,
}

#[pymethods]
impl ClassicalFilter {
    /// Build a filter sized for `expected_items` items at target false-positive
    /// rate `false_pos`. Optional `seed` makes hashing deterministic; without
    /// one the OS RNG seeds the hasher.
    #[staticmethod]
    #[pyo3(signature = (false_pos, expected_items, *, seed=None))]
    fn from_false_pos(false_pos: f64, expected_items: usize, seed: Option<u64>) -> PyResult<Self> {
        check_fp(false_pos)?;
        check_items(expected_items)?;
        let inner = match seed {
            Some(s) => Filter::<Classical>::with_false_pos(false_pos)
                .seed(s as u128)
                .expected_items(expected_items),
            None => Filter::<Classical>::with_false_pos(false_pos).expected_items(expected_items),
        };
        Ok(Self { inner })
    }

    /// Build a filter with explicit `num_bits` and either `expected_items`
    /// (to derive `k`) or `hashes` (to set `k` directly). Pass exactly one.
    #[staticmethod]
    #[pyo3(signature = (num_bits, *, expected_items=None, hashes=None, seed=None))]
    fn from_num_bits(
        num_bits: usize,
        expected_items: Option<usize>,
        hashes: Option<u32>,
        seed: Option<u64>,
    ) -> PyResult<Self> {
        if num_bits == 0 {
            return Err(PyValueError::new_err("num_bits must be > 0"));
        }
        match (expected_items, hashes) {
            (Some(_), Some(_)) | (None, None) => Err(PyValueError::new_err(
                "pass exactly one of expected_items or hashes",
            )),
            (Some(n), None) => {
                check_items(n)?;
                let builder = Filter::<Classical>::with_num_bits(num_bits);
                let inner = match seed {
                    Some(s) => builder.seed(s as u128).expected_items(n),
                    None => builder.expected_items(n),
                };
                Ok(Self { inner })
            }
            (None, Some(k)) => {
                let builder = Filter::<Classical>::with_num_bits(num_bits);
                let inner = match seed {
                    Some(s) => builder.seed(s as u128).hashes(k),
                    None => builder.hashes(k),
                };
                Ok(Self { inner })
            }
        }
    }

    /// Insert `item` (bytes, bytearray, memoryview, or str). Returns `True`
    /// if all probe bits were already set (i.e. the item was probably
    /// already present).
    fn insert(&self, item: BytesOrStr) -> bool {
        self.inner.insert(item.as_bytes())
    }

    /// Membership test. False positives are possible at the rate this filter
    /// was sized for; false negatives are not.
    fn contains(&self, item: BytesOrStr) -> bool {
        self.inner.contains(item.as_bytes())
    }

    /// `item in filter` — Python's `__contains__` protocol.
    fn __contains__(&self, item: BytesOrStr) -> bool {
        self.inner.contains(item.as_bytes())
    }

    /// Reset every bit to zero.
    fn clear(&mut self) {
        self.inner.clear();
    }

    /// Estimated count of distinct items inserted.
    fn estimate_count(&self) -> usize {
        self.inner.estimate_count()
    }

    #[getter]
    fn num_bits(&self) -> usize {
        self.inner.num_bits()
    }

    #[getter]
    fn num_hashes(&self) -> u32 {
        self.inner.num_hashes()
    }

    fn __repr__(&self) -> String {
        format!(
            "ClassicalFilter(num_bits={}, num_hashes={})",
            self.inner.num_bits(),
            self.inner.num_hashes(),
        )
    }
}

// ---------------------------------------------------------------------------
// AtomicFilter
// ---------------------------------------------------------------------------

/// Concurrent Bloom filter: multiple threads can call `insert` against the
/// same instance. This is the headline reason to use the Rust binding from
/// Python — `insert_many` releases the GIL.
#[pyclass(name = "AtomicFilter")]
struct AtomicFilter {
    inner: Filter<Atomic>,
}

#[pymethods]
impl AtomicFilter {
    #[staticmethod]
    #[pyo3(signature = (false_pos, expected_items, *, seed=None))]
    fn from_false_pos(false_pos: f64, expected_items: usize, seed: Option<u64>) -> PyResult<Self> {
        check_fp(false_pos)?;
        check_items(expected_items)?;
        let inner = match seed {
            Some(s) => Filter::<Atomic>::with_false_pos(false_pos)
                .seed(s as u128)
                .expected_items(expected_items),
            None => Filter::<Atomic>::with_false_pos(false_pos).expected_items(expected_items),
        };
        Ok(Self { inner })
    }

    fn insert(&self, item: BytesOrStr) -> bool {
        self.inner.insert(item.as_bytes())
    }

    fn contains(&self, item: BytesOrStr) -> bool {
        self.inner.contains(item.as_bytes())
    }

    fn __contains__(&self, item: BytesOrStr) -> bool {
        self.inner.contains(item.as_bytes())
    }

    /// Bulk insert. Releases the GIL while running, so other Python threads
    /// can make progress. Returns the number of items that were already
    /// fully present (probable duplicates).
    fn insert_many(&self, py: Python<'_>, items: Vec<OwnedBytesOrStr>) -> usize {
        py.allow_threads(|| {
            items
                .iter()
                .filter(|item| self.inner.insert(item.as_bytes()))
                .count()
        })
    }

    fn clear(&mut self) {
        self.inner.clear();
    }

    fn estimate_count(&self) -> usize {
        self.inner.estimate_count()
    }

    #[getter]
    fn num_bits(&self) -> usize {
        self.inner.num_bits()
    }

    #[getter]
    fn num_hashes(&self) -> u32 {
        self.inner.num_hashes()
    }

    fn __repr__(&self) -> String {
        format!(
            "AtomicFilter(num_bits={}, num_hashes={})",
            self.inner.num_bits(),
            self.inner.num_hashes(),
        )
    }
}

// ---------------------------------------------------------------------------
// BlockedFilter
// ---------------------------------------------------------------------------

/// Single-threaded Bloom filter with a 512-bit blocked layout. Faster
/// `insert` at sizes that exceed L1 on x86; on Apple silicon the win is
/// modest because the memory subsystem already hides scattered-miss latency.
#[pyclass(name = "BlockedFilter")]
struct BlockedFilter {
    inner: Filter<Blocked>,
}

#[pymethods]
impl BlockedFilter {
    #[staticmethod]
    #[pyo3(signature = (false_pos, expected_items, *, seed=None))]
    fn from_false_pos(false_pos: f64, expected_items: usize, seed: Option<u64>) -> PyResult<Self> {
        check_fp(false_pos)?;
        check_items(expected_items)?;
        let inner = match seed {
            Some(s) => Filter::<Blocked>::with_false_pos(false_pos)
                .seed(s as u128)
                .expected_items(expected_items),
            None => Filter::<Blocked>::with_false_pos(false_pos).expected_items(expected_items),
        };
        Ok(Self { inner })
    }

    fn insert(&self, item: BytesOrStr) -> bool {
        self.inner.insert(item.as_bytes())
    }

    fn contains(&self, item: BytesOrStr) -> bool {
        self.inner.contains(item.as_bytes())
    }

    fn __contains__(&self, item: BytesOrStr) -> bool {
        self.inner.contains(item.as_bytes())
    }

    fn clear(&mut self) {
        self.inner.clear();
    }

    fn estimate_count(&self) -> usize {
        self.inner.estimate_count()
    }

    #[getter]
    fn num_bits(&self) -> usize {
        self.inner.num_bits()
    }

    #[getter]
    fn num_hashes(&self) -> u32 {
        self.inner.num_hashes()
    }

    fn __repr__(&self) -> String {
        format!(
            "BlockedFilter(num_bits={}, num_hashes={})",
            self.inner.num_bits(),
            self.inner.num_hashes(),
        )
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

#[pymodule]
fn bouquet(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ClassicalFilter>()?;
    m.add_class::<AtomicFilter>()?;
    m.add_class::<BlockedFilter>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
