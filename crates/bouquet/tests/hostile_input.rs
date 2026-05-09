//! Hostile-input tests: feed malformed, malicious, or adversarial blobs at
//! the deserializer and assert it returns a clean error instead of panicking
//! or accepting a filter that would explode at query time.
//!
//! These tests exist because three real bugs were observed in audited Bloom
//! crates:
//!
//! 1. Empty `bits` array → every query indexes into a zero-length slice.
//! 2. `num_hashes == u32::MAX` → 4-billion-iteration probe loop per query.
//! 3. `num_bits` inconsistent with `bits.len() * 64` → wrong indexing math,
//!    silent data corruption.

#![cfg(all(feature = "classical", feature = "serde"))]

use bouquet::{Classical, Filter};

/// Minimal valid filter, for "what fields look like in the wild" reference.
fn baseline_json() -> String {
    let f = Filter::<Classical>::with_false_pos(0.01)
        .seed(0)
        .expected_items(100);
    serde_json::to_string(&f).expect("serialize baseline")
}

#[test]
fn rejects_empty_bits() {
    let blob = r#"{
        "num_bits": 0,
        "num_hashes": 7,
        "hasher": { "key0": 0, "key1": 0 },
        "bits": []
    }"#;
    let r: Result<Filter<Classical>, _> = serde_json::from_str(blob);
    let err = r.expect_err("empty bits must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("zero bits") || msg.contains("EmptyBits"),
        "unexpected error: {msg}"
    );
}

#[test]
fn rejects_giant_num_hashes() {
    // bits.len()*64 = 128, so num_bits must be 128 to even pass that check.
    let blob = r#"{
        "num_bits": 128,
        "num_hashes": 4294967295,
        "hasher": { "key0": 0, "key1": 0 },
        "bits": [0, 0]
    }"#;
    let r: Result<Filter<Classical>, _> = serde_json::from_str(blob);
    let err = r.expect_err("u32::MAX num_hashes must be rejected");
    assert!(
        err.to_string().contains("out of range"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_zero_num_hashes() {
    let blob = r#"{
        "num_bits": 64,
        "num_hashes": 0,
        "hasher": { "key0": 0, "key1": 0 },
        "bits": [0]
    }"#;
    let r: Result<Filter<Classical>, _> = serde_json::from_str(blob);
    assert!(r.is_err());
}

#[test]
fn rejects_inconsistent_num_bits() {
    let blob = r#"{
        "num_bits": 1024,
        "num_hashes": 7,
        "hasher": { "key0": 0, "key1": 0 },
        "bits": [0, 0]
    }"#;
    let r: Result<Filter<Classical>, _> = serde_json::from_str(blob);
    let err = r.expect_err("inconsistent num_bits must be rejected");
    assert!(
        err.to_string().contains("does not match"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_truncated_json() {
    let r: Result<Filter<Classical>, _> = serde_json::from_str(r#"{"num_bits": 64,"#);
    assert!(r.is_err());
}

#[test]
fn rejects_garbage_json() {
    let r: Result<Filter<Classical>, _> = serde_json::from_str("not json at all");
    assert!(r.is_err());
}

#[test]
fn rejects_truncated_cbor() {
    let bad_cbor: Vec<u8> = vec![0x82, 0x01]; // start of a 2-element array, no payload
    let r: Result<Filter<Classical>, _> = ciborium::de::from_reader(bad_cbor.as_slice());
    assert!(r.is_err());
}

#[test]
fn rejects_random_cbor() {
    let r: Result<Filter<Classical>, _> = ciborium::de::from_reader(&[0xff_u8, 0xff, 0xff][..]);
    assert!(r.is_err());
}

#[test]
fn baseline_round_trips_so_we_know_we_are_not_just_returning_errors() {
    let blob = baseline_json();
    let r: Result<Filter<Classical>, _> = serde_json::from_str(&blob);
    assert!(r.is_ok(), "baseline JSON must round-trip: {:?}", r.err());
}
