//! Unit tests for posting list encode/decode, intersection, and union.

use ripline::posting::{
    intersection, union, varint_decode, varint_encode, PostingList, ROARING_THRESHOLD,
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

// ---------------------------------------------------------------------------
// PostingList threshold switching
// ---------------------------------------------------------------------------

#[test]
fn small_posting_list_below_threshold() {
    let ids: Vec<u32> = (0..100).collect();
    let pl = PostingList::from_sorted(&ids);
    assert!(matches!(pl, PostingList::Small(_)));
    assert_eq!(pl.to_vec().unwrap(), ids);
}

#[test]
fn large_posting_list_at_threshold() {
    let ids: Vec<u32> = (0..ROARING_THRESHOLD as u32).collect();
    let pl = PostingList::from_sorted(&ids);
    assert!(matches!(pl, PostingList::Large(_)));
    assert_eq!(pl.to_vec().unwrap(), ids);
}

#[test]
fn posting_list_len() {
    let ids: Vec<u32> = (0u32..50).collect();
    let pl = PostingList::from_sorted(&ids);
    assert_eq!(pl.len(), 50);
    assert!(!pl.is_empty());
}

#[test]
fn posting_list_empty() {
    let pl = PostingList::from_sorted(&[]);
    assert!(pl.is_empty());
}

// ---------------------------------------------------------------------------
// Intersection correctness
// ---------------------------------------------------------------------------

fn small_list(ids: &[u32]) -> PostingList {
    PostingList::from_sorted(ids)
}

#[test]
fn intersection_equal_size() {
    let a = small_list(&[1, 3, 5, 7, 9]);
    let b = small_list(&[1, 2, 5, 8, 9]);
    let result = intersection(&[a, b]).unwrap();
    assert_eq!(result, [1, 5, 9]);
}

#[test]
fn intersection_disjoint() {
    let a = small_list(&[1, 2, 3]);
    let b = small_list(&[4, 5, 6]);
    let result = intersection(&[a, b]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn intersection_one_empty() {
    let a = small_list(&[1, 2, 3]);
    let b = small_list(&[]);
    let result = intersection(&[a, b]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn intersection_single_list() {
    let ids = vec![1u32, 2, 3];
    let a = small_list(&ids);
    assert_eq!(intersection(&[a]).unwrap(), ids);
}

#[test]
fn intersection_empty_input() {
    assert_eq!(intersection(&[]).unwrap(), Vec::<u32>::new());
}

#[test]
fn intersection_three_lists() {
    let a = small_list(&[1, 2, 3, 4, 5]);
    let b = small_list(&[2, 3, 4]);
    let c = small_list(&[3, 4, 5, 6]);
    assert_eq!(intersection(&[a, b, c]).unwrap(), [3, 4]);
}

#[test]
fn intersection_skewed_uses_galloping() {
    // Ratio > 32: [0..1000] vs [100, 500] → galloping path
    let large: Vec<u32> = (0u32..1000).collect();
    let small_ids = vec![100u32, 500];
    let a = small_list(&large);
    let b = small_list(&small_ids);
    assert_eq!(intersection(&[a, b]).unwrap(), [100u32, 500]);
}

// ---------------------------------------------------------------------------
// Union correctness
// ---------------------------------------------------------------------------

#[test]
fn union_disjoint() {
    let a = small_list(&[1, 3, 5]);
    let b = small_list(&[2, 4, 6]);
    assert_eq!(union(&[a, b]).unwrap(), [1, 2, 3, 4, 5, 6]);
}

#[test]
fn union_overlapping() {
    let a = small_list(&[1, 2, 5]);
    let b = small_list(&[2, 3, 5]);
    assert_eq!(union(&[a, b]).unwrap(), [1, 2, 3, 5]);
}

#[test]
fn union_one_empty() {
    let a = small_list(&[1, 2, 3]);
    let b = small_list(&[]);
    assert_eq!(union(&[a, b]).unwrap(), [1, 2, 3]);
}

#[test]
fn union_empty_input() {
    assert_eq!(union(&[]).unwrap(), Vec::<u32>::new());
}

#[test]
fn union_three_lists_deduped() {
    let a = small_list(&[1, 2]);
    let b = small_list(&[2, 3]);
    let c = small_list(&[3, 4]);
    assert_eq!(union(&[a, b, c]).unwrap(), [1, 2, 3, 4]);
}

#[test]
fn union_result_is_sorted() {
    let a = small_list(&[10, 20, 30]);
    let b = small_list(&[5, 15, 25]);
    let result = union(&[a, b]).unwrap();
    assert!(result.windows(2).all(|w| w[0] < w[1]), "union must be sorted");
}
