//! Validating `Serialize` / `Deserialize` for [`Filter`].
//!
//! The on-wire form is a self-describing struct with `num_bits`, `num_hashes`,
//! `hasher`, and `bits` fields. Self-describing formats (JSON, CBOR) round
//! trip cleanly; non-self-describing formats (bincode) honour the field
//! order declared here.
//!
//! Deserialization is *manual* and *defensive*. The brief calls out three
//! real bugs from auditing other Bloom-filter crates:
//!
//! - empty `bits` (every query panics on indexing),
//! - `num_hashes` set to `u32::MAX` (4-billion-iteration probe loop),
//! - `num_bits` inconsistent with `bits.len() * 64` (silent corruption).
//!
//! Each is rejected with a typed [`crate::Error`] surfaced through serde's
//! custom error mechanism.

use alloc::vec::Vec;
use core::fmt;
use core::hash::BuildHasher;
use core::marker::PhantomData;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::backend::Backend;
use crate::math::MAX_HASHES;
use crate::{Error, Filter};

const FIELDS: &[&str] = &["num_bits", "num_hashes", "hasher", "bits"];

impl<B, S> Serialize for Filter<B, S>
where
    B: Backend,
    S: BuildHasher + Serialize,
{
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        let mut state = serializer.serialize_struct("Filter", FIELDS.len())?;
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
    NumBits,
    NumHashes,
    Hasher,
    Bits,
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

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut num_bits: Option<usize> = None;
        let mut num_hashes: Option<u32> = None;
        let mut hasher: Option<S> = None;
        let mut bits: Option<Vec<u64>> = None;

        while let Some(key) = map.next_key::<Field>()? {
            match key {
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

        let num_bits = num_bits.ok_or_else(|| de::Error::missing_field("num_bits"))?;
        let num_hashes = num_hashes.ok_or_else(|| de::Error::missing_field("num_hashes"))?;
        let hasher = hasher.ok_or_else(|| de::Error::missing_field("hasher"))?;
        let bits = bits.ok_or_else(|| de::Error::missing_field("bits"))?;

        validate(num_bits, num_hashes, &bits).map_err(de::Error::custom)?;
        let backend = B::from_words(bits).ok_or_else(|| de::Error::custom(Error::EmptyBits))?;

        Ok(Filter::from_validated_parts(backend, num_hashes, hasher))
    }
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
