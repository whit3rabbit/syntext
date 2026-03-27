//! Roaring bitmap utilities for posting list serialization.
//!
//! Handles serialization and deserialization of `RoaringBitmap` to/from
//! byte slices using the official Roaring serialization format (compatible
//! with the `roaring` crate and interoperable with other Roaring implementations).

use roaring::RoaringBitmap;
use std::io::Cursor;

/// Serialize a `RoaringBitmap` to bytes.
///
/// Uses the standard Roaring serialization format (portable, little-endian).
pub fn serialize(bm: &RoaringBitmap) -> Vec<u8> {
    let mut buf = Vec::with_capacity(bm.serialized_size());
    bm.serialize_into(&mut buf)
        .expect("RoaringBitmap serialization to Vec should never fail");
    buf
}

/// Deserialize a `RoaringBitmap` from bytes.
///
/// Returns an error if the bytes are not valid Roaring format.
pub fn deserialize(bytes: &[u8]) -> Result<RoaringBitmap, String> {
    RoaringBitmap::deserialize_from(Cursor::new(bytes))
        .map_err(|e| format!("RoaringBitmap deserialize: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty() {
        let bm = RoaringBitmap::new();
        let bytes = serialize(&bm);
        let got = deserialize(&bytes).unwrap();
        assert_eq!(got, bm);
    }

    #[test]
    fn round_trip_dense() {
        let mut bm = RoaringBitmap::new();
        for i in 0..10_000u32 {
            bm.insert(i);
        }
        let bytes = serialize(&bm);
        let got = deserialize(&bytes).unwrap();
        assert_eq!(got, bm);
    }

    #[test]
    fn round_trip_sparse() {
        let mut bm = RoaringBitmap::new();
        bm.insert(0);
        bm.insert(1_000_000);
        bm.insert(u32::MAX);
        let bytes = serialize(&bm);
        let got = deserialize(&bytes).unwrap();
        assert_eq!(got, bm);
    }

    #[test]
    fn invalid_bytes_returns_error() {
        let result = deserialize(b"not a roaring bitmap");
        assert!(result.is_err());
    }
}
