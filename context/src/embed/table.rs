//! Vector sidecar files: one per segment and embedder.
//!
//! `seg-<generation>-<id>.<embedder key>.vec` holds one row per passage of
//! `seg-<generation>-<id>.ctxs`, in passage order:
//!
//! ```text
//! header   magic "SXCTXVEC", version u32, dims u32, rows u32, id length u32, id bytes
//! rows     text hash u64, scale f32, dims x i8      (hash 0 = not embedded)
//! footer   xxh64 of everything before it, u64
//! ```
//!
//! Little-endian throughout. Sidecars are a cache: one that is missing,
//! stale, or fails validation is ignored (and rebuilt by the next
//! `embed`), never trusted and never fatal.

use xxhash_rust::xxh64::xxh64;

use super::{MAX_DIMS, MAX_ID_BYTES};
use crate::error::{ContextError, Result};

const MAGIC: &[u8; 8] = b"SXCTXVEC";
const VERSION: u32 = 1;
const HEADER_FIXED: usize = 8 + 4 * 4;
const FOOTER: usize = 8;

/// The vectors of one segment's passages for one embedder.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VectorTable {
    pub id: String,
    pub dims: usize,
    hashes: Vec<u64>,
    scales: Vec<f32>,
    data: Vec<i8>,
}

impl VectorTable {
    /// A table with `rows` rows, none embedded.
    pub(crate) fn empty(id: &str, dims: usize, rows: usize) -> Self {
        VectorTable {
            id: id.to_string(),
            dims,
            hashes: vec![0; rows],
            scales: vec![0.0; rows],
            data: vec![0; rows * dims],
        }
    }

    pub(crate) fn rows(&self) -> usize {
        self.hashes.len()
    }

    /// Rows holding a vector.
    pub(crate) fn present(&self) -> usize {
        self.hashes.iter().filter(|&&h| h != 0).count()
    }

    /// Text hash, scale, and vector of `row`, when it is embedded.
    pub(crate) fn get(&self, row: usize) -> Option<(u64, f32, &[i8])> {
        let h = *self.hashes.get(row)?;
        (h != 0).then(|| {
            (
                h,
                self.scales[row],
                &self.data[row * self.dims..(row + 1) * self.dims],
            )
        })
    }

    /// Store a vector for `row`.
    pub(crate) fn set(&mut self, row: usize, hash: u64, scale: f32, q: &[i8]) {
        debug_assert_eq!(q.len(), self.dims);
        self.hashes[row] = hash;
        self.scales[row] = scale;
        self.data[row * self.dims..(row + 1) * self.dims].copy_from_slice(q);
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let row = 12 + self.dims;
        let mut out = Vec::with_capacity(HEADER_FIXED + self.id.len() + self.rows() * row + FOOTER);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.dims as u32).to_le_bytes());
        out.extend_from_slice(&(self.rows() as u32).to_le_bytes());
        out.extend_from_slice(&(self.id.len() as u32).to_le_bytes());
        out.extend_from_slice(self.id.as_bytes());
        for r in 0..self.rows() {
            out.extend_from_slice(&self.hashes[r].to_le_bytes());
            out.extend_from_slice(&self.scales[r].to_le_bytes());
            out.extend(
                self.data[r * self.dims..(r + 1) * self.dims]
                    .iter()
                    .map(|&b| b as u8),
            );
        }
        let sum = xxh64(&out, 0);
        out.extend_from_slice(&sum.to_le_bytes());
        out
    }

    /// Parse and validate a sidecar for `id` covering exactly `rows`
    /// passages.
    pub(crate) fn decode(bytes: &[u8], id: &str, rows: usize) -> Result<Self> {
        let bad = |why: &str| ContextError::corrupt(format!("vector sidecar: {why}"));
        if bytes.len() < HEADER_FIXED + FOOTER || &bytes[..8] != MAGIC {
            return Err(bad("bad magic"));
        }
        let (body, footer) = bytes.split_at(bytes.len() - FOOTER);
        let sum = u64::from_le_bytes(footer.try_into().expect("8 bytes"));
        if xxh64(body, 0) != sum {
            return Err(bad("checksum mismatch"));
        }
        let u32_at = |off: usize| u32::from_le_bytes(body[off..off + 4].try_into().expect("4"));
        if u32_at(8) != VERSION {
            return Err(bad("unsupported version"));
        }
        let dims = u32_at(12) as usize;
        let stored_rows = u32_at(16) as usize;
        let id_len = u32_at(20) as usize;
        if dims == 0 || dims > MAX_DIMS || id_len > MAX_ID_BYTES {
            return Err(bad("bad header"));
        }
        if stored_rows != rows {
            return Err(bad("row count does not match the segment"));
        }
        let id_end = HEADER_FIXED + id_len;
        let row = 12 + dims;
        if body.len() != id_end + rows.saturating_mul(row) {
            return Err(bad("length does not match the header"));
        }
        if &body[HEADER_FIXED..id_end] != id.as_bytes() {
            return Err(bad("written for a different embedder"));
        }
        let mut t = VectorTable::empty(id, dims, rows);
        for r in 0..rows {
            let off = id_end + r * row;
            t.hashes[r] = u64::from_le_bytes(body[off..off + 8].try_into().expect("8"));
            let scale = f32::from_le_bytes(body[off + 8..off + 12].try_into().expect("4"));
            if t.hashes[r] != 0 && !(scale.is_finite() && scale > 0.0) {
                return Err(bad("bad scale"));
            }
            t.scales[r] = scale;
            for (d, &b) in body[off + 12..off + row].iter().enumerate() {
                t.data[r * dims + d] = b as i8;
            }
        }
        Ok(t)
    }
}

/// The embedder id a sidecar declares, before validation (`decode` checks
/// it against everything else).
pub(crate) fn sidecar_id(bytes: &[u8]) -> Option<String> {
    if bytes.len() < HEADER_FIXED || &bytes[..8] != MAGIC {
        return None;
    }
    let id_len = u32::from_le_bytes(bytes[20..24].try_into().ok()?) as usize;
    if id_len > MAX_ID_BYTES || bytes.len() < HEADER_FIXED + id_len {
        return None;
    }
    String::from_utf8(bytes[HEADER_FIXED..HEADER_FIXED + id_len].to_vec()).ok()
}

/// Sidecar file name of `segment_file` for embedder `key`.
pub(crate) fn sidecar_name(segment_file: &str, key: &str) -> String {
    format!("{}.{key}.vec", segment_file.trim_end_matches(".ctxs"))
}

/// The segment file a sidecar belongs to, for a well-formed sidecar name.
pub(crate) fn sidecar_segment(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".vec")?;
    let (seg, key) = stem.rsplit_once('.')?;
    let valid_key = key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit());
    (valid_key && seg.starts_with("seg-")).then(|| format!("{seg}.ctxs"))
}

/// The embedder key of a well-formed sidecar name.
pub(crate) fn sidecar_key(name: &str) -> Option<&str> {
    sidecar_segment(name)?;
    name.strip_suffix(".vec")?.rsplit_once('.').map(|(_, k)| k)
}
