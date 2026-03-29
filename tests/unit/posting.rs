//! Unit tests for posting list encode/decode, intersection, and union.

use syntext::posting::{varint_decode, varint_encode, PostingList};

// ---------------------------------------------------------------------------
// Delta-varint encode / decode round-trips
// ---------------------------------------------------------------------------

#[test]
fn varint_empty() {
    let ids: Vec<u32> = vec![];
    assert_eq!(varint_decode(&varint_encode(&ids).unwrap()).unwrap(), ids);
}

#[test]
fn varint_single_zero() {
    assert_eq!(
        varint_decode(&varint_encode(&[0u32]).unwrap()).unwrap(),
        [0u32]
    );
}

#[test]
fn varint_sequential() {
    let ids: Vec<u32> = (0u32..1000).collect();
    assert_eq!(varint_decode(&varint_encode(&ids).unwrap()).unwrap(), ids);
}

#[test]
fn varint_large_deltas() {
    let ids = vec![0u32, 1_000_000, 2_000_000, u32::MAX / 2, u32::MAX];
    assert_eq!(varint_decode(&varint_encode(&ids).unwrap()).unwrap(), ids);
}

#[test]
fn varint_max_value() {
    assert_eq!(
        varint_decode(&varint_encode(&[u32::MAX]).unwrap()).unwrap(),
        [u32::MAX]
    );
}

#[test]
fn varint_unsorted_returns_error() {
    assert_eq!(
        varint_encode(&[5, 3, 7]),
        Err("varint_encode: ids must be strictly ascending (no duplicates)")
    );
}

#[test]
fn varint_bad_bytes_returns_error() {
    // Truncated: continuation bit set but no next byte
    assert!(varint_decode(&[0x80u8]).is_err());
}

// ---------------------------------------------------------------------------
// PostingList::is_empty fast path
// ---------------------------------------------------------------------------

#[test]
fn posting_list_is_empty_small() {
    let empty = PostingList::Small(vec![]);
    assert!(empty.is_empty());

    let non_empty = PostingList::Small(varint_encode(&[1, 2, 3]).unwrap());
    assert!(!non_empty.is_empty());
}

#[test]
fn posting_list_is_empty_large() {
    use roaring::RoaringBitmap;

    let empty = PostingList::Large(RoaringBitmap::new());
    assert!(empty.is_empty());

    let mut bm = RoaringBitmap::new();
    bm.insert(42);
    let non_empty = PostingList::Large(bm);
    assert!(!non_empty.is_empty());
}
