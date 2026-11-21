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
        "layout": "flat",
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
        "layout": "flat",
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
        "layout": "flat",
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
        "layout": "flat",
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

/// A blocked backend needs a whole number of 512-bit blocks. A word count
/// that passes the generic `num_bits == bits.len() * 64` check but isn't a
/// multiple of 8 words must be rejected — with the layout error, not the
/// misleading "zero bits" it used to map to.
#[cfg(feature = "blocked")]
#[test]
fn rejects_partial_block_for_blocked_backend() {
    use bouquet::Blocked;

    let blob = r#"{
        "layout": "blocked512",
        "num_bits": 576,
        "num_hashes": 7,
        "hasher": { "key0": 0, "key1": 0 },
        "bits": [0, 0, 0, 0, 0, 0, 0, 0, 0]
    }"#;
    let r: Result<Filter<Blocked>, _> = serde_json::from_str(blob);
    let err = r.expect_err("9 words is not a whole number of blocks");
    assert!(
        err.to_string()
            .contains("not valid for this backend's layout"),
        "unexpected error: {err}"
    );
}

/// The headline reason `layout` is on the wire. A `Classical` blob has a
/// word count that satisfies the blocked backends' checks too, so before the
/// tag existed this deserialized *successfully* and then reported "not
/// present" for every item the filter actually contained.
#[cfg(feature = "blocked")]
#[test]
fn rejects_flat_blob_loaded_as_blocked() {
    use bouquet::Blocked;

    // 4096 bits = 64 words = a whole number of 512-bit blocks, so nothing
    // but the layout tag can catch this.
    let f = Filter::<Classical>::with_num_bits(4096)
        .seed(0)
        .hashes(7)
        .clone();
    for i in 0..200u32 {
        f.insert(&i);
    }
    let blob = serde_json::to_string(&f).expect("serialize");

    let r: Result<Filter<Blocked>, _> = serde_json::from_str(&blob);
    let err = r.expect_err("a flat blob must not load as a blocked filter");
    assert!(
        err.to_string().contains("false negatives"),
        "unexpected error: {err}"
    );
}

/// ...and the reverse direction.
#[cfg(feature = "blocked")]
#[test]
fn rejects_blocked_blob_loaded_as_flat() {
    use bouquet::Blocked;

    let f = Filter::<Blocked>::with_num_bits(4096).seed(0).hashes(7);
    for i in 0..200u32 {
        f.insert(&i);
    }
    let blob = serde_json::to_string(&f).expect("serialize");

    let r: Result<Filter<Classical>, _> = serde_json::from_str(&blob);
    assert!(r.is_err(), "a blocked blob must not load as a flat filter");
}

/// Backends that genuinely share a layout must stay interchangeable — the
/// tag is a compatibility key, not a backend name.
#[cfg(all(feature = "blocked", feature = "simd"))]
#[test]
fn blocked_and_simd_blocked_remain_interchangeable() {
    use bouquet::{Blocked, SimdBlocked};

    let f = Filter::<Blocked>::with_num_bits(4096).seed(0).hashes(7);
    for i in 0..200u32 {
        f.insert(&i);
    }
    let blob = serde_json::to_string(&f).expect("serialize");

    let loaded: Filter<SimdBlocked> =
        serde_json::from_str(&blob).expect("same layout must load cleanly");
    for i in 0..200u32 {
        assert!(loaded.contains(&i), "false negative for {i} after reload");
    }
}

/// An untagged blob is refused outright rather than assumed compatible.
#[test]
fn rejects_missing_layout() {
    let blob = r#"{
        "num_bits": 64,
        "num_hashes": 7,
        "hasher": { "key0": 0, "key1": 0 },
        "bits": [0]
    }"#;
    let r: Result<Filter<Classical>, _> = serde_json::from_str(blob);
    let err = r.expect_err("an untagged blob must be rejected");
    assert!(
        err.to_string().contains("layout"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_unknown_layout() {
    let blob = r#"{
        "layout": "quantum",
        "num_bits": 64,
        "num_hashes": 7,
        "hasher": { "key0": 0, "key1": 0 },
        "bits": [0]
    }"#;
    let r: Result<Filter<Classical>, _> = serde_json::from_str(blob);
    assert!(r.is_err(), "an unknown layout tag must be rejected");
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
