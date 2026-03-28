//! Posting list encoding, decoding, intersection, and union.
//!
//! # Encoding tiers
//!
//! Two representations, chosen per posting list at write time:
//!
//! - **Delta-varint** (`PostingList::Small`): sorted `u32` doc IDs, stored as
//!   delta-encoded variable-length integers. Compact for lists with <8K entries.
//!   1-2 bytes/entry typical, 5 bytes worst-case.
//!
//! - **Roaring bitmap** (`PostingList::Large`): for lists with >=8K entries.
//!   Automatically adapts to density (run-length, bitset, or array containers).
//!
//! # Operations
//!
//! - `intersection`: AND over multiple posting lists. Adaptive: linear merge
//!   for similarly-sized lists, galloping for size ratio >32:1.
//! - `union`: OR over multiple posting lists via k-way min-heap merge.

pub mod roaring_util;

use roaring::RoaringBitmap;

/// Threshold (number of doc IDs) above which Roaring bitmap is used.
pub const ROARING_THRESHOLD: usize = 8192;

// ---------------------------------------------------------------------------
// T016: Delta-varint encoding / decoding
// ---------------------------------------------------------------------------

/// Encode a sorted slice of `u32` doc IDs as delta-varint bytes.
///
/// Each entry is stored as a variable-length integer (1-5 bytes) encoding the
/// delta from the previous value. The first entry uses the value directly.
///
/// # Panics
///
/// Panics if `ids` is not sorted. Callers must guarantee sorted, deduplicated input.
pub fn varint_encode(ids: &[u32]) -> Vec<u8> {
    assert!(
        ids.windows(2).all(|w| w[0] <= w[1]),
        "varint_encode: ids must be sorted (caller contract violation)"
    );

    let mut out = Vec::with_capacity(ids.len() * 2);
    let mut prev = 0u32;
    for &id in ids {
        let delta = id - prev;
        write_varint(delta, &mut out);
        prev = id;
    }
    out
}

/// Decode delta-varint encoded bytes back to a `Vec<u32>` doc IDs.
///
/// Returns an error string if the encoding is malformed.
pub fn varint_decode(bytes: &[u8]) -> Result<Vec<u32>, &'static str> {
    let mut ids = Vec::new();
    let mut pos = 0usize;
    let mut prev = 0u32;

    while pos < bytes.len() {
        let (delta, consumed) = read_varint(&bytes[pos..])?;
        // For non-first entries, delta must be > 0 to maintain strict ascending order.
        if !ids.is_empty() && delta == 0 {
            return Err("delta-varint duplicate: zero delta produces non-ascending sequence");
        }
        prev = prev.checked_add(delta).ok_or("delta-varint overflow")?;
        ids.push(prev);
        pos += consumed;
    }
    Ok(ids)
}

/// Write a single `u32` as a variable-length integer (1-5 bytes).
///
/// Encoding: 7 bits of data per byte. The MSB is a continuation bit:
/// 1 = more bytes follow, 0 = last byte of this value.
#[inline]
fn write_varint(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        } else {
            out.push(byte | 0x80);
        }
    }
}

/// Read a single variable-length integer from `bytes`.
///
/// Returns `(value, bytes_consumed)` or an error if the encoding is invalid.
#[inline]
fn read_varint(bytes: &[u8]) -> Result<(u32, usize), &'static str> {
    let mut value = 0u32;
    let mut shift = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        if shift >= 35 {
            return Err("varint: too many continuation bytes");
        }
        let bits = (byte & 0x7F) as u32;
        // 5th byte (shift=28): only bottom 4 bits are valid for u32
        if shift == 28 && bits > 0x0F {
            return Err("varint: u32 overflow");
        }
        value |= bits << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    Err("varint: unexpected end of input")
}

// ---------------------------------------------------------------------------
// T017: Posting list enum (Small = delta-varint, Large = Roaring)
// ---------------------------------------------------------------------------

/// A posting list: either delta-varint encoded (small) or a Roaring bitmap (large).
#[derive(Debug, Clone)]
pub enum PostingList {
    /// Delta-varint encoded; used for lists with fewer than `ROARING_THRESHOLD` entries.
    Small(Vec<u8>),
    /// Roaring bitmap; used for lists with `ROARING_THRESHOLD` or more entries.
    Large(RoaringBitmap),
}

impl PostingList {
    /// Decode this posting list to a sorted `Vec<u32>`.
    pub fn to_vec(&self) -> Result<Vec<u32>, &'static str> {
        match self {
            PostingList::Small(bytes) => varint_decode(bytes),
            PostingList::Large(bm) => Ok(bm.iter().collect()),
        }
    }

    /// Number of entries in this posting list.
    ///
    /// **Warning:** O(n) for `Small` variant (fully decodes the varint stream).
    /// For cardinality checks during search, use `MmapSegment::gram_cardinality()`
    /// which reads the stored entry_count from the dictionary in O(1).
    pub fn len(&self) -> usize {
        match self {
            PostingList::Small(bytes) => {
                // Count entries by decoding (no stored length)
                varint_decode(bytes).map(|v| v.len()).unwrap_or(0)
            }
            PostingList::Large(bm) => bm.len() as usize,
        }
    }

    /// Returns true if this posting list is empty.
    ///
    /// O(1) for both variants (checks byte-slice length for `Small`,
    /// bitmap length for `Large`).
    pub fn is_empty(&self) -> bool {
        match self {
            PostingList::Small(bytes) => bytes.is_empty(),
            PostingList::Large(bm) => bm.is_empty(),
        }
    }
}

// ---------------------------------------------------------------------------
// T018: Adaptive intersection
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Inline tests (unit tests in tests/unit/posting.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip_empty() {
        let ids: Vec<u32> = vec![];
        assert_eq!(varint_decode(&varint_encode(&ids)).unwrap(), ids);
    }

    #[test]
    fn varint_round_trip_single() {
        let ids = vec![42u32];
        assert_eq!(varint_decode(&varint_encode(&ids)).unwrap(), ids);
    }

    #[test]
    fn varint_round_trip_sequential() {
        let ids: Vec<u32> = (0u32..100).collect();
        assert_eq!(varint_decode(&varint_encode(&ids)).unwrap(), ids);
    }

    #[test]
    fn varint_round_trip_large_deltas() {
        let ids = vec![0u32, 1_000_000, 2_000_000, u32::MAX - 1, u32::MAX];
        assert_eq!(varint_decode(&varint_encode(&ids)).unwrap(), ids);
    }

    #[test]
    fn varint_decode_rejects_zero_delta_duplicate() {
        // varint(5) then varint(0) → [5, 0]
        let bytes = [5u8, 0u8];
        let result = varint_decode(&bytes);
        assert!(result.is_err(), "zero delta (duplicate id) must be rejected: {result:?}");
    }

    #[test]
    fn varint_decode_first_entry_zero_is_ok() {
        // First entry with value 0 (delta=0 from prev=0) is valid.
        let bytes = varint_encode(&[0u32, 1u32, 2u32]);
        let result = varint_decode(&bytes).unwrap();
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    #[should_panic(expected = "varint_encode: ids must be sorted")]
    fn varint_encode_panics_on_unsorted_input_in_release() {
        // This must panic in both debug and release builds.
        varint_encode(&[5, 3, 7]);
    }
}
