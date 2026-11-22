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

use ::bouquet::{Atomic, Backend, Blocked, Classical, Error as BouquetError, Filter};
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::{PyMemoryError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::pybacked::{PyBackedBytes, PyBackedStr};
use pyo3::types::{PyByteArray, PyBytes, PyString};
use pyo3::Borrowed;

/// Largest filter a binding call may request, in bits: 2^36 = 8 GiB.
///
/// Rust's global allocator calls `handle_alloc_error` on failure, which
/// aborts the process — from Python that means the interpreter dies with
/// SIGABRT and no traceback, which no `except` clause can catch. The core
/// crate's `try_*` constructors turn *allocator* failure into a value, but a
/// request this side of plausible can still make the OS OOM-killer step in
/// before `malloc` ever returns null. So we refuse the absurd up front and
/// use the fallible constructors for everything else.
const MAX_BITS: usize = 1 << 36;

/// Map a core-crate error onto the Python exception a caller would expect.
fn map_bouquet_err(e: BouquetError) -> PyErr {
    match e {
        BouquetError::AllocationFailed { bits } => PyMemoryError::new_err(format!(
            "could not allocate a {bits}-bit filter ({} GiB)",
            bits / 8 / (1 << 30)
        )),
        BouquetError::CapacityOverflow { .. } => {
            PyValueError::new_err(format!("filter size is too large to represent: {e}"))
        }
        BouquetError::InvalidHashes(k) => {
            PyValueError::new_err(format!("hashes must be in 1..=1024, got {k}"))
        }
        // `bouquet::Error` is `#[non_exhaustive]`; anything new is still a
        // validation failure, so ValueError is the right default.
        other => PyValueError::new_err(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Input dispatch: accept bytes-like or str
// ---------------------------------------------------------------------------

fn item_type_error(obj: &Bound<'_, PyAny>) -> PyErr {
    PyTypeError::new_err(format!(
        "expected str or a bytes-like object (bytes, bytearray, memoryview), got {}",
        obj.get_type()
    ))
}

/// One item argument, for the single-item methods and `insert_many`.
/// Extraction tries, in order: `Str` covers `str` (CPython's UTF-8 cache,
/// zero-copy when already encoded); `Bytes` covers `bytes` (zero-copy —
/// `PyBackedBytes` keeps a Python reference) and `bytearray` (copied,
/// since it is mutable); `Other` is the buffer-protocol
/// fallback for `memoryview` and the like, which pays one allocation
/// since arbitrary buffer objects don't give us a stable borrow. All
/// arms hash the same underlying bytes, so `f.insert("hello")` and
/// `f.insert(b"hello")` produce identical filter state.
///
/// Extraction is a manual impl rather than `#[derive(FromPyObject)]`
/// because a derived `Vec<u8>` fallback arm also accepts arbitrary
/// sequences of ints — `f.insert([104, 105])` would silently coerce to
/// `b"hi"`. `PyBuffer` restricts the fallback to real buffer-protocol
/// objects.
enum BytesOrStr {
    Bytes(PyBackedBytes),
    Str(PyBackedStr),
    Other(Vec<u8>),
}

impl<'a, 'py> FromPyObject<'a, 'py> for BytesOrStr {
    type Error = PyErr;

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> PyResult<Self> {
        let obj = &*obj;
        // `str` first: it is the common argument, and trying `PyBackedBytes`
        // on one builds and discards a Python `TypeError` on every call.
        //
        // Dispatch `str` on its *type*, not on whether extraction succeeds.
        // Extraction fails for a `str` holding a lone surrogate (routine
        // from `os.fsdecode`, `surrogateescape` decoding, or `json.loads`
        // of malformed input), and swallowing that error used to drop
        // through to the buffer arm and report "expected str ..., got
        // <class 'str'>". Let Python's own UnicodeEncodeError surface.
        if obj.is_instance_of::<PyString>() {
            return obj.extract::<PyBackedStr>().map(Self::Str);
        }
        if let Ok(b) = obj.extract::<PyBackedBytes>() {
            return Ok(Self::Bytes(b));
        }
        let buf = PyBuffer::<u8>::get(obj).map_err(|_| item_type_error(obj))?;
        Ok(Self::Other(buf.to_vec(obj.py())?))
    }
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

/// Validate an explicit `num_bits` before it reaches the allocator.
fn check_num_bits(num_bits: usize) -> PyResult<()> {
    if num_bits == 0 {
        return Err(PyValueError::new_err("num_bits must be > 0"));
    }
    if num_bits > MAX_BITS {
        return Err(PyValueError::new_err(format!(
            "num_bits must be <= {MAX_BITS} (2^36 bits = 8 GiB), got {num_bits}"
        )));
    }
    Ok(())
}

/// Validate an explicit `hashes` (`k`).
///
/// The core builder *panics* on an out-of-range `k`, and a panic crossing
/// the FFI boundary becomes `pyo3_runtime.PanicException`, which derives
/// from `BaseException` — so a caller's `except Exception:` does not catch
/// it and the raw Rust panic message hits stderr. Check here instead.
fn check_hashes(k: u32) -> PyResult<()> {
    if k == 0 || k > 1024 {
        return Err(PyValueError::new_err(format!(
            "hashes must be in 1..=1024, got {k}"
        )));
    }
    Ok(())
}

/// Reject an `expected_items`/`false_pos` pair whose implied size is beyond
/// what we are willing to attempt.
///
/// Checked *before* construction, because the sizing math itself succeeds
/// for values that would then ask the allocator for terabytes.
fn check_implied_size(expected_items: usize, false_pos: f64) -> PyResult<()> {
    let bits = ::bouquet::math::optimal_num_bits(expected_items, false_pos).ok_or_else(|| {
        PyValueError::new_err(format!(
            "expected_items={expected_items} at false_pos={false_pos} implies a filter \
             size too large to represent"
        ))
    })?;
    if bits > MAX_BITS {
        return Err(PyValueError::new_err(format!(
            "expected_items={expected_items} at false_pos={false_pos} implies a \
             {bits}-bit filter, above the {MAX_BITS}-bit (8 GiB) ceiling"
        )));
    }
    Ok(())
}

/// Shared body of every class's `from_num_bits`.
fn build_from_num_bits<B: Backend>(
    num_bits: usize,
    expected_items: Option<usize>,
    hashes: Option<u32>,
    seed: Option<u128>,
) -> PyResult<Filter<B>> {
    check_num_bits(num_bits)?;
    let builder = Filter::<B>::with_num_bits(num_bits);
    let builder = match seed {
        Some(s) => builder.seed(s),
        None => builder,
    };
    match (expected_items, hashes) {
        (Some(_), Some(_)) | (None, None) => Err(PyValueError::new_err(
            "pass exactly one of expected_items or hashes",
        )),
        (Some(n), None) => {
            check_items(n)?;
            builder.try_expected_items(n).map_err(map_bouquet_err)
        }
        (None, Some(k)) => {
            check_hashes(k)?;
            builder.try_hashes(k).map_err(map_bouquet_err)
        }
    }
}

/// Extract the argument of `insert_many` / `contains_many`.
///
/// A bytes-like or str argument would otherwise be iterated as a sequence —
/// of ints for bytes, of 1-char strs for str — and report the baffling
/// "expected str or a bytes-like object, got <class 'int'>" about an
/// argument that *is* bytes-like.
///
/// `BytesOrStr` borrows `str` and `bytes` payloads instead of copying them:
/// both are immutable, and the backing references are `Send`, so the slices
/// stay valid if the caller releases the GIL. (`bytearray` is mutable, so
/// `PyBackedBytes` copies it.)
fn extract_items(method: &str, items: &Bound<'_, PyAny>) -> PyResult<Vec<BytesOrStr>> {
    if items.is_instance_of::<PyBytes>()
        || items.is_instance_of::<PyByteArray>()
        || items.is_instance_of::<PyString>()
    {
        return Err(PyTypeError::new_err(format!(
            "{method} expects a sequence of items, got a single {} — \
             wrap it in a list to pass one item",
            items.get_type()
        )));
    }
    items.extract()
}

/// Leading bytes of a serialized filter: a tag, then a format version.
///
/// The version byte is what lets the encoding change later without old
/// blobs being misread: an unknown version is rejected up front.
const WIRE_MAGIC: &[u8; 4] = b"BQT\x01";

/// Shared body of every class's `to_bytes`.
fn dump<'py, B: Backend>(py: Python<'py>, filter: &Filter<B>) -> PyResult<Bound<'py, PyBytes>> {
    let mut out = WIRE_MAGIC.to_vec();
    out = postcard::to_extend(filter, out)
        .map_err(|e| PyValueError::new_err(format!("could not serialize filter: {e}")))?;
    Ok(PyBytes::new(py, &out))
}

/// Shared body of every class's `from_bytes`.
///
/// The core deserializer validates the payload (layout tag, hash count,
/// word count against the claimed size), so a corrupt or mismatched blob is
/// a `ValueError`, never a filter that answers wrongly.
fn load<B: Backend>(data: &[u8]) -> PyResult<Filter<B>> {
    let payload = data.strip_prefix(WIRE_MAGIC.as_slice()).ok_or_else(|| {
        PyValueError::new_err(
            "not a bouquet filter: bad header (wrong data, or written by an \
             incompatible version)",
        )
    })?;
    // postcard discards the text of serde's custom errors, so the core
    // deserializer's typed rejections all surface as one opaque message.
    // Read the leading layout tag here to give the one mistake a caller is
    // likely to make — loading into the wrong class — a useful error.
    let corrupt = |e: postcard::Error| {
        PyValueError::new_err(format!(
            "could not load filter: corrupt or truncated data ({e})"
        ))
    };
    let (layout, _) = postcard::take_from_bytes::<&str>(payload).map_err(corrupt)?;
    if layout != B::LAYOUT.as_str() {
        return Err(PyValueError::new_err(format!(
            "filter was saved with layout '{layout}' but this class uses '{}'; \
             loading it would produce false negatives",
            B::LAYOUT
        )));
    }
    postcard::from_bytes(payload).map_err(corrupt)
}

// ---------------------------------------------------------------------------
// ClassicalFilter
// ---------------------------------------------------------------------------

/// Single-threaded Bloom filter with a flat textbook layout.
///
/// Construct with `ClassicalFilter.from_false_pos(fp, expected_items)` (or
/// `.from_num_bits(...)`). Methods accept `bytes` / `bytearray` /
/// `memoryview` or `str` (UTF-8 encoded).
// `unsendable`: the underlying backend stores its words in `Cell`, so it
// is `!Sync` by construction — that is the whole point of the
// single-threaded backends. pyo3 requires `Sync` for a normal
// `#[pyclass]` because free-threaded CPython can hand one object to two
// threads at once; `unsendable` instead pins the object to the thread
// that created it and raises if another thread touches it. Callers who
// need cross-thread sharing want `AtomicFilter`.
#[pyclass(name = "ClassicalFilter", unsendable)]
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
    fn from_false_pos(false_pos: f64, expected_items: usize, seed: Option<u128>) -> PyResult<Self> {
        check_fp(false_pos)?;
        check_items(expected_items)?;
        check_implied_size(expected_items, false_pos)?;
        let builder = Filter::<Classical>::with_false_pos(false_pos);
        let inner = match seed {
            Some(s) => builder.seed(s).try_expected_items(expected_items),
            None => builder.try_expected_items(expected_items),
        }
        .map_err(map_bouquet_err)?;
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
        seed: Option<u128>,
    ) -> PyResult<Self> {
        build_from_num_bits(num_bits, expected_items, hashes, seed).map(|inner| Self { inner })
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

    /// Bulk insert from a sequence (list or tuple) of items. Returns the
    /// number of items that were already fully present (probable
    /// duplicates). One call instead of one per item; the GIL stays held,
    /// because this filter is single-threaded.
    fn insert_many(&self, items: &Bound<'_, PyAny>) -> PyResult<usize> {
        let items = extract_items("insert_many", items)?;
        Ok(self
            .inner
            .insert_all(items.iter().map(BytesOrStr::as_bytes)))
    }

    /// Bulk membership test. Returns one `bool` per item, in order.
    fn contains_many(&self, items: &Bound<'_, PyAny>) -> PyResult<Vec<bool>> {
        let items = extract_items("contains_many", items)?;
        Ok(items
            .iter()
            .map(|item| self.inner.contains(item.as_bytes()))
            .collect())
    }

    /// Reset every bit to zero.
    fn clear(&mut self) {
        self.inner.clear();
    }

    /// Estimated count of distinct items inserted. Returns `2**64 - 1` on a
    /// fully saturated filter; check `is_saturated()` first.
    fn estimate_count(&self) -> usize {
        self.inner.estimate_count()
    }

    /// Whether every bit is set. A saturated filter answers `True` to every
    /// membership test, and `estimate_count()` returns `2**64 - 1` for it
    /// because the estimator has no finite answer there.
    fn is_saturated(&self) -> bool {
        self.inner.is_saturated()
    }

    /// Serialize the filter, including its hash key, to `bytes`.
    ///
    /// The key is needed to query the filter after loading, so anyone who
    /// can read these bytes can test membership and craft colliding
    /// inputs. Keep them as private as the data the filter describes.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        dump(py, &self.inner)
    }

    /// Rebuild a filter from `to_bytes` output. Raises `ValueError` on
    /// anything else, including bytes written by a class with a different
    /// bit layout. Only load bytes you trust.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        load(data).map(|inner| Self { inner })
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
    fn from_false_pos(false_pos: f64, expected_items: usize, seed: Option<u128>) -> PyResult<Self> {
        check_fp(false_pos)?;
        check_items(expected_items)?;
        check_implied_size(expected_items, false_pos)?;
        let builder = Filter::<Atomic>::with_false_pos(false_pos);
        let inner = match seed {
            Some(s) => builder.seed(s).try_expected_items(expected_items),
            None => builder.try_expected_items(expected_items),
        }
        .map_err(map_bouquet_err)?;
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
        seed: Option<u128>,
    ) -> PyResult<Self> {
        build_from_num_bits(num_bits, expected_items, hashes, seed).map(|inner| Self { inner })
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

    /// Bulk insert from a sequence (list or tuple) of items. Releases the
    /// GIL while running, so other Python threads can make progress.
    /// Returns the number of items that were already fully present
    /// (probable duplicates).
    ///
    /// Note this takes a *sequence of items*, not a single item. Passing
    /// `bytes` or `str` directly raises `TypeError` rather than iterating
    /// it element-wise.
    fn insert_many(&self, py: Python<'_>, items: &Bound<'_, PyAny>) -> PyResult<usize> {
        let items = extract_items("insert_many", items)?;
        // The borrowed items are dropped after `detach` returns, with the GIL
        // held again.
        Ok(py.detach(|| {
            items
                .iter()
                .filter(|item| self.inner.insert(item.as_bytes()))
                .count()
        }))
    }

    /// Bulk membership test. Returns one `bool` per item, in order.
    /// Releases the GIL while running, like `insert_many`.
    fn contains_many(&self, py: Python<'_>, items: &Bound<'_, PyAny>) -> PyResult<Vec<bool>> {
        let items = extract_items("contains_many", items)?;
        Ok(py.detach(|| {
            items
                .iter()
                .map(|item| self.inner.contains(item.as_bytes()))
                .collect()
        }))
    }

    fn clear(&mut self) {
        self.inner.clear();
    }

    fn estimate_count(&self) -> usize {
        self.inner.estimate_count()
    }

    /// Whether every bit is set. A saturated filter answers `True` to every
    /// membership test, and `estimate_count()` returns `2**64 - 1` for it
    /// because the estimator has no finite answer there.
    fn is_saturated(&self) -> bool {
        self.inner.is_saturated()
    }

    /// Serialize the filter, including its hash key, to `bytes`.
    ///
    /// The key is needed to query the filter after loading, so anyone who
    /// can read these bytes can test membership and craft colliding
    /// inputs. Keep them as private as the data the filter describes.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        dump(py, &self.inner)
    }

    /// Rebuild a filter from `to_bytes` output. Raises `ValueError` on
    /// anything else, including bytes written by a class with a different
    /// bit layout. Only load bytes you trust.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        load(data).map(|inner| Self { inner })
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
// `unsendable`: the underlying backend stores its words in `Cell`, so it
// is `!Sync` by construction — that is the whole point of the
// single-threaded backends. pyo3 requires `Sync` for a normal
// `#[pyclass]` because free-threaded CPython can hand one object to two
// threads at once; `unsendable` instead pins the object to the thread
// that created it and raises if another thread touches it. Callers who
// need cross-thread sharing want `AtomicFilter`.
#[pyclass(name = "BlockedFilter", unsendable)]
struct BlockedFilter {
    inner: Filter<Blocked>,
}

#[pymethods]
impl BlockedFilter {
    #[staticmethod]
    #[pyo3(signature = (false_pos, expected_items, *, seed=None))]
    fn from_false_pos(false_pos: f64, expected_items: usize, seed: Option<u128>) -> PyResult<Self> {
        check_fp(false_pos)?;
        check_items(expected_items)?;
        check_implied_size(expected_items, false_pos)?;
        let builder = Filter::<Blocked>::with_false_pos(false_pos);
        let inner = match seed {
            Some(s) => builder.seed(s).try_expected_items(expected_items),
            None => builder.try_expected_items(expected_items),
        }
        .map_err(map_bouquet_err)?;
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
        seed: Option<u128>,
    ) -> PyResult<Self> {
        build_from_num_bits(num_bits, expected_items, hashes, seed).map(|inner| Self { inner })
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

    /// Bulk insert from a sequence (list or tuple) of items. Returns the
    /// number of items that were already fully present (probable
    /// duplicates). One call instead of one per item; the GIL stays held,
    /// because this filter is single-threaded.
    fn insert_many(&self, items: &Bound<'_, PyAny>) -> PyResult<usize> {
        let items = extract_items("insert_many", items)?;
        Ok(self
            .inner
            .insert_all(items.iter().map(BytesOrStr::as_bytes)))
    }

    /// Bulk membership test. Returns one `bool` per item, in order.
    fn contains_many(&self, items: &Bound<'_, PyAny>) -> PyResult<Vec<bool>> {
        let items = extract_items("contains_many", items)?;
        Ok(items
            .iter()
            .map(|item| self.inner.contains(item.as_bytes()))
            .collect())
    }

    fn clear(&mut self) {
        self.inner.clear();
    }

    fn estimate_count(&self) -> usize {
        self.inner.estimate_count()
    }

    /// Whether every bit is set. A saturated filter answers `True` to every
    /// membership test, and `estimate_count()` returns `2**64 - 1` for it
    /// because the estimator has no finite answer there.
    fn is_saturated(&self) -> bool {
        self.inner.is_saturated()
    }

    /// Serialize the filter, including its hash key, to `bytes`.
    ///
    /// The key is needed to query the filter after loading, so anyone who
    /// can read these bytes can test membership and craft colliding
    /// inputs. Keep them as private as the data the filter describes.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        dump(py, &self.inner)
    }

    /// Rebuild a filter from `to_bytes` output. Raises `ValueError` on
    /// anything else, including bytes written by a class with a different
    /// bit layout. Only load bytes you trust.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        load(data).map(|inner| Self { inner })
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
