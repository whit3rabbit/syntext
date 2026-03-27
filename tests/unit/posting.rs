//! Unit tests for posting list encode/decode, intersection, and union.

use ripline_rs::posting::{
    varint_decode, varint_encode,
};

// ---------------------------------------------------------------------------
// Delta-varint encode / decode round-trips
// ---------------------------------------------------------------------------

#[test]
fn varint_empty() {
    let ids: Vec<u32> = vec![];
    assert_eq!(varint_decode(&varint_encode(&ids)).unwrap(), ids);
}

#[test]
fn varint_single_zero() {
    assert_eq!(varint_decode(&varint_encode(&[0u32])).unwrap(), [0u32]);
}

#[test]
fn varint_sequential() {
    let ids: Vec<u32> = (0u32..1000).collect();
    assert_eq!(varint_decode(&varint_encode(&ids)).unwrap(), ids);
}

#[test]
fn varint_large_deltas() {
    let ids = vec![0u32, 1_000_000, 2_000_000, u32::MAX / 2, u32::MAX];
    assert_eq!(varint_decode(&varint_encode(&ids)).unwrap(), ids);
}

#[test]
fn varint_max_value() {
    assert_eq!(
        varint_decode(&varint_encode(&[u32::MAX])).unwrap(),
        [u32::MAX]
    );
}

#[test]
fn varint_bad_bytes_returns_error() {
    // Truncated: continuation bit set but no next byte
    assert!(varint_decode(&[0x80u8]).is_err());
}
