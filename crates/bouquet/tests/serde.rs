//! Round-trip tests for the serde implementation.
//!
//! Two visitor paths need coverage, because they are entirely separate code:
//!
//! - Self-describing formats (JSON, CBOR) drive `visit_map`.
//! - Non-self-describing formats (such as postcard) drive `visit_seq` and
//!   rely on the declared field order.
//!
//! The crate documented compact binary format support while implementing only `visit_map`,
//! so serialization succeeded and deserialization always failed — a
//! deployment persisting to a compact binary format wrote data it could
//! never read back. These tests pin both directions.

#![cfg(all(feature = "classical", feature = "serde"))]

use bouquet::{Classical, Filter};

#[cfg(feature = "atomic")]
use bouquet::Atomic;

fn fixture() -> Filter<Classical> {
    let f = Filter::<Classical>::with_false_pos(0.001)
        .seed(0xdead_beef_cafe_babe)
        .expected_items(5_000);
    for i in 0..1_000u32 {
        f.insert(&i);
    }
    f
}

#[test]
fn json_round_trip_preserves_membership() {
    let f = fixture();
    let json = serde_json::to_string(&f).expect("serialize");
    let g: Filter<Classical> = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(g.num_bits(), f.num_bits());
    assert_eq!(g.num_hashes(), f.num_hashes());
    for i in 0..1_000u32 {
        assert!(g.contains(&i), "missing {i} after JSON round trip");
    }
    // Spot-check a few non-inserted items return the same answer.
    for i in 100_000u32..100_010 {
        assert_eq!(g.contains(&i), f.contains(&i), "diverged on {i}");
    }
}

#[test]
fn cbor_round_trip_preserves_membership() {
    let f = fixture();
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&f, &mut buf).expect("serialize");
    let g: Filter<Classical> = ciborium::de::from_reader(buf.as_slice()).expect("deserialize");

    assert_eq!(g.num_bits(), f.num_bits());
    assert_eq!(g.num_hashes(), f.num_hashes());
    for i in 0..1_000u32 {
        assert!(g.contains(&i), "missing {i} after CBOR round trip");
    }
}

/// Non-self-describing: exercises `visit_seq`.
#[test]
fn postcard_round_trip_preserves_membership() {
    let f = fixture();
    let buf = postcard::to_allocvec(&f).expect("serialize");
    let g: Filter<Classical> = postcard::from_bytes(&buf).expect("deserialize");

    assert_eq!(g.num_bits(), f.num_bits());
    assert_eq!(g.num_hashes(), f.num_hashes());
    for i in 0..1_000u32 {
        assert!(g.contains(&i), "missing {i} after postcard round trip");
    }
}

/// The layout guard has to hold on the `visit_seq` path too, not just
/// `visit_map` — it is a separate function and could easily skip the check.
#[cfg(feature = "blocked")]
#[test]
fn postcard_rejects_layout_mismatch() {
    use bouquet::Blocked;

    let f = fixture();
    let buf = postcard::to_allocvec(&f).expect("serialize");
    let r: Result<Filter<Blocked>, _> = postcard::from_bytes(&buf);
    assert!(
        r.is_err(),
        "visit_seq must enforce the layout tag like visit_map does"
    );
}

#[test]
fn round_trip_preserves_compatibility() {
    let f = fixture();
    let json = serde_json::to_string(&f).expect("serialize");
    let g: Filter<Classical> = serde_json::from_str(&json).expect("deserialize");
    assert!(f.is_compatible(&g));
}

#[cfg(feature = "atomic")]
#[test]
fn atomic_json_round_trip() {
    let f = Filter::<Atomic>::with_false_pos(0.001)
        .seed(0x1234_5678)
        .expected_items(2_000);
    for i in 0..500u32 {
        f.insert(&i);
    }

    let json = serde_json::to_string(&f).expect("serialize");
    let g: Filter<Atomic> = serde_json::from_str(&json).expect("deserialize");

    for i in 0..500u32 {
        assert!(g.contains(&i));
    }
}
