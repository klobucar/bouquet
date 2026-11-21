//! Validating `Serialize` / `Deserialize` for [`Filter`].
//!
//! The on-wire form is a struct with `layout`, `num_bits`, `num_hashes`,
//! `hasher`, and `bits` fields, in that order. Self-describing formats
//! (JSON, CBOR) round trip through [`Visitor::visit_map`]; non-self-describing
//! formats (bincode, postcard) round trip through [`Visitor::visit_seq`] and
//! depend on that field order.
//!
//! Deserialization is *manual* and *defensive*. It rejects:
//!
//! - a `layout` that doesn't match the target backend (see below),
//! - empty `bits` (every query panics on indexing),
//! - `num_hashes` set to `u32::MAX` (4-billion-iteration probe loop),
//! - `num_bits` inconsistent with `bits.len() * 64` (silent corruption),
//! - a word count that doesn't fit the backend's layout (partial blocks).
//!
//! Each is rejected with a typed [`crate::Error`] surfaced through serde's
//! custom error mechanism.
//!
//! ## Why `layout` is on the wire
//!
//! Nothing about a bare word array says which probe scheme wrote it. A
//! `Filter<Classical>` blob has a word count that satisfies the blocked
//! backends' checks too, so without a tag it deserializes *successfully* as
//! a `Filter<Blocked>` and then reports "not present" for every item it
//! actually contains — false negatives, which a Bloom filter is supposed to
//! make impossible. The tag makes that a clean
//! [`crate::Error::LayoutMismatch`] instead.
//!
//! This also guards the reverse direction: a backend whose probe derivation
//! changes between releases must change its [`Layout`] tag, so old blobs
//! fail loudly rather than silently mis-answering.
//!
//! ## The hasher key is part of the wire format
//!
//! [`crate::DefaultHasher`] serializes its SipHash key — it has to, because
//! the key is required to query the filter after deserialization. The
//! consequence: anyone who can read a persisted filter can construct inputs
//! that collide in it, so the HashDoS resistance of a random key only holds
//! while the serialized form stays private. Treat persisted filters with the
//! same confidentiality as the key itself.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::hash::BuildHasher;
use core::marker::PhantomData;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::backend::Backend;
use crate::math::MAX_HASHES;
use crate::{Error, Filter};

const FIELDS: &[&str] = &["layout", "num_bits", "num_hashes", "hasher", "bits"];

impl<B, S> Serialize for Filter<B, S>
where
    B: Backend,
    S: BuildHasher + Serialize,
{
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        let mut state = serializer.serialize_struct("Filter", FIELDS.len())?;
        state.serialize_field("layout", B::LAYOUT.as_str())?;
        state.serialize_field("num_bits", &self.backend.num_bits())?;
        state.serialize_field("num_hashes", &self.num_hashes)?;
        state.serialize_field("hasher", &self.hasher)?;
        // Snapshot once. For `Atomic` this is a `Relaxed`-loaded copy; if
        // another thread is inserting concurrently, the snapshot represents
        // *some* valid prefix-state of the filter.
        let words = self.backend.snapshot_words();
        state.serialize_field("bits", &words)?;
        state.end()
    }
}

impl<'de, B, S> Deserialize<'de> for Filter<B, S>
where
    B: Backend,
    S: BuildHasher + Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_struct("Filter", FIELDS, FilterVisitor::<B, S>(PhantomData))
    }
}

struct FilterVisitor<B, S>(PhantomData<fn() -> (B, S)>);

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Field {
    Layout,
    NumBits,
    NumHashes,
    Hasher,
    Bits,
}

/// Check a wire tag against the target backend's layout.
fn check_layout<B: Backend>(found: &str) -> Result<(), Error> {
    if found == B::LAYOUT.as_str() {
        Ok(())
    } else {
        Err(Error::LayoutMismatch {
            expected: B::LAYOUT.as_str(),
            found: found.to_string(),
        })
    }
}

impl<'de, B, S> Visitor<'de> for FilterVisitor<B, S>
where
    B: Backend,
    S: BuildHasher + Deserialize<'de>,
{
    type Value = Filter<B, S>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a bouquet Filter struct")
    }

    /// Non-self-describing formats (bincode, postcard) drive the visitor
    /// with a sequence in declared field order. Without this the crate could
    /// *write* those formats but never read them back.
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let layout: String = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        check_layout::<B>(&layout).map_err(de::Error::custom)?;

        let num_bits: usize = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(1, &self))?;
        let num_hashes: u32 = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(2, &self))?;
        let hasher: S = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(3, &self))?;
        let bits: Vec<u64> = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(4, &self))?;

        finish::<B, S, A::Error>(num_bits, num_hashes, hasher, bits)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut layout: Option<String> = None;
        let mut num_bits: Option<usize> = None;
        let mut num_hashes: Option<u32> = None;
        let mut hasher: Option<S> = None;
        let mut bits: Option<Vec<u64>> = None;

        while let Some(key) = map.next_key::<Field>()? {
            match key {
                Field::Layout => {
                    if layout.is_some() {
                        return Err(de::Error::duplicate_field("layout"));
                    }
                    let found: String = map.next_value()?;
                    // Check eagerly: on a mismatch there is no point
                    // decoding a potentially large `bits` array.
                    check_layout::<B>(&found).map_err(de::Error::custom)?;
                    layout = Some(found);
                }
                Field::NumBits => {
                    if num_bits.is_some() {
                        return Err(de::Error::duplicate_field("num_bits"));
                    }
                    num_bits = Some(map.next_value()?);
                }
                Field::NumHashes => {
                    if num_hashes.is_some() {
                        return Err(de::Error::duplicate_field("num_hashes"));
                    }
                    num_hashes = Some(map.next_value()?);
                }
                Field::Hasher => {
                    if hasher.is_some() {
                        return Err(de::Error::duplicate_field("hasher"));
                    }
                    hasher = Some(map.next_value()?);
                }
                Field::Bits => {
                    if bits.is_some() {
                        return Err(de::Error::duplicate_field("bits"));
                    }
                    bits = Some(map.next_value()?);
                }
            }
        }

        let _ = layout.ok_or_else(|| de::Error::missing_field("layout"))?;
        let num_bits = num_bits.ok_or_else(|| de::Error::missing_field("num_bits"))?;
        let num_hashes = num_hashes.ok_or_else(|| de::Error::missing_field("num_hashes"))?;
        let hasher = hasher.ok_or_else(|| de::Error::missing_field("hasher"))?;
        let bits = bits.ok_or_else(|| de::Error::missing_field("bits"))?;

        finish::<B, S, A::Error>(num_bits, num_hashes, hasher, bits)
    }
}

/// Shared tail of `visit_seq` and `visit_map`: validate, rebuild, wrap.
fn finish<B, S, E>(
    num_bits: usize,
    num_hashes: u32,
    hasher: S,
    bits: Vec<u64>,
) -> Result<Filter<B, S>, E>
where
    B: Backend,
    S: BuildHasher,
    E: de::Error,
{
    validate(num_bits, num_hashes, &bits).map_err(de::Error::custom)?;
    // `validate` already rejected empty `bits`, so a `from_words` failure
    // here means the word count doesn't fit the backend's layout (e.g. not
    // a whole number of blocks for `Blocked`).
    let n_words = bits.len();
    let backend =
        B::from_words(bits).ok_or_else(|| de::Error::custom(Error::InvalidWordCount(n_words)))?;

    // Same clamp the constructor applies, so a blob claiming a `k` this
    // backend cannot use loads as the `k` it will actually run.
    let num_hashes = B::clamp_hashes(num_hashes);

    Ok(Filter::from_validated_parts(backend, num_hashes, hasher))
}

fn validate(num_bits: usize, num_hashes: u32, bits: &[u64]) -> Result<(), Error> {
    if bits.is_empty() {
        return Err(Error::EmptyBits);
    }
    if num_hashes == 0 || num_hashes > MAX_HASHES {
        return Err(Error::InvalidHashes(num_hashes));
    }
    let derived = bits
        .len()
        .checked_mul(64)
        .ok_or(Error::InconsistentNumBits {
            claimed: num_bits,
            derived: usize::MAX,
        })?;
    if num_bits != derived {
        return Err(Error::InconsistentNumBits {
            claimed: num_bits,
            derived,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(num_bits: usize, num_hashes: u32, bits: &[u64]) {
        validate(num_bits, num_hashes, bits).expect("should validate");
    }

    fn err(num_bits: usize, num_hashes: u32, bits: &[u64]) -> Error {
        validate(num_bits, num_hashes, bits).expect_err("should reject")
    }

    #[test]
    fn rejects_empty_bits() {
        assert_eq!(err(0, 7, &[]), Error::EmptyBits);
    }

    #[test]
    fn rejects_zero_hashes() {
        assert_eq!(err(64, 0, &[0]), Error::InvalidHashes(0));
    }

    #[test]
    fn rejects_giant_hashes() {
        assert_eq!(
            err(64, MAX_HASHES + 1, &[0]),
            Error::InvalidHashes(MAX_HASHES + 1)
        );
    }

    #[test]
    fn rejects_inconsistent_num_bits() {
        let e = err(128, 7, &[0, 0, 0]);
        assert!(matches!(e, Error::InconsistentNumBits { .. }));
    }

    #[test]
    fn accepts_valid() {
        ok(192, 7, &[0, 0, 0]);
    }
}
