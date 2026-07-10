//! On-disk sidecar for `PathIndex`: `paths.idx`.
//!
//! `Index::open` currently rebuilds `PathIndex` (and its extension/component
//! Roaring bitmaps) from scratch on every open by iterating every doc entry in
//! every base segment. Since `auto_update_budget_ms` reopens/updates the index
//! on every search, that fixed rebuild cost is paid far more often than a
//! traditional single-shot `st index` workflow.
//!
//! `paths.idx` caches the sorted path list plus the extension/component
//! bitmaps computed by `PathIndex::build`, checksummed so a torn or corrupted
//! write is detected rather than silently trusted. It is written by
//! `build.rs` and `compact.rs` immediately after each rewrites the base
//! segments and manifest. Loading it (with fallback to the existing rebuild
//! path on any failure) is wired into `open.rs` separately.
//!
//! # Format (little-endian)
//!
//! ```text
//! magic:           4 bytes, b"STPI"
//! version:         u32
//! checksum:        u64             xxh64 of every byte that follows this field
//! path_count:      u32
//! paths:           path_count * (u32 len, `len` bytes, UTF-8, forward-slash separated)
//! ext_count:       u32
//! extensions:      ext_count * (u32 key_len, key bytes, u32 bitmap_len, roaring bytes)
//! component_count: u32
//! components:      component_count * (u32 key_len, key bytes, u32 bitmap_len, roaring bytes)
//! ```
//!
//! Extension/component keys are the same ASCII-lowercased byte strings used as
//! `HashMap` keys in `PathIndex`.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use roaring::RoaringBitmap;
use xxhash_rust::xxh64::xxh64;

use crate::path::PathIndex;
use crate::path_util::{path_bytes, path_from_bytes};
use crate::posting::roaring_util;

/// Filename of the sidecar, relative to the index directory.
pub(crate) const PATHS_IDX_FILENAME: &str = "paths.idx";

const MAGIC: &[u8; 4] = b"STPI";
pub(crate) const FORMAT_VERSION: u32 = 1;

/// Fixed header size: 4-byte magic + u32 version + u64 checksum.
const HEADER_LEN: usize = 4 + 4 + 8;

/// Reject a sidecar bigger than this before allocating any decode buffers.
/// A 100M-path repo with average 40-byte paths is well under 8 GB; this cap
/// exists purely to bound a corrupt/adversarial file, not real corpora.
pub(crate) const MAX_SIDECAR_SIZE: u64 = 8 * 1024 * 1024 * 1024;

/// Cap on the initial `Vec`/`HashMap` capacity reserved from a
/// length-prefixed count read out of the file. Untrusted input can claim an
/// arbitrarily large count (up to `u32::MAX`) while the file itself is tiny;
/// without this cap, reading the 4-byte count would drive a multi-GB
/// allocation before the truncated body is ever noticed. Real corpora are
/// far below this, so it never affects a valid sidecar; a corrupt/adversarial
/// one instead fails with `SidecarError::Truncated` once the loop runs past
/// the actual (small) body.
const MAX_PREALLOC_ENTRIES: usize = 1 << 20;

/// Reasons a `paths.idx` sidecar was rejected. Every variant means the same
/// thing to the caller: fall back to rebuilding `PathIndex` from segment doc
/// tables. The variants exist so a verbose caller can log *why* the fixed
/// per-open cache was skipped, rather than silently eating its benefit.
#[derive(Debug)]
pub(crate) enum SidecarError {
    /// The file is missing, or a read/metadata syscall failed.
    Io(io::Error),
    /// On-disk size exceeds `MAX_SIDECAR_SIZE`, checked before any read.
    TooLarge(u64),
    /// Fewer than `HEADER_LEN` bytes: not even a full header.
    TooShort,
    /// First 4 bytes are not `b"STPI"`.
    BadMagic,
    /// Version field does not match `FORMAT_VERSION`.
    UnsupportedVersion(u32),
    /// xxh64 over the body does not match the stored checksum.
    ChecksumMismatch,
    /// A length-prefixed field pointed past the end of the body.
    Truncated,
    /// A roaring bitmap payload failed to deserialize.
    Bitmap(String),
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidecarError::Io(e) => write!(f, "I/O error reading paths.idx: {e}"),
            SidecarError::TooLarge(n) => write!(
                f,
                "paths.idx is {n} bytes, exceeds {MAX_SIDECAR_SIZE}-byte safety cap"
            ),
            SidecarError::TooShort => write!(f, "paths.idx is shorter than its fixed header"),
            SidecarError::BadMagic => write!(f, "paths.idx has an invalid magic number"),
            SidecarError::UnsupportedVersion(v) => write!(
                f,
                "paths.idx format version {v} is not supported (expected {FORMAT_VERSION})"
            ),
            SidecarError::ChecksumMismatch => {
                write!(f, "paths.idx checksum does not match its contents")
            }
            SidecarError::Truncated => {
                write!(
                    f,
                    "paths.idx body ends before all recorded entries were read"
                )
            }
            SidecarError::Bitmap(e) => write!(f, "paths.idx has a corrupt roaring bitmap: {e}"),
        }
    }
}

/// Load and validate `dir/paths.idx`, returning the decoded `PathIndex`.
///
/// Returns `Err` (never panics, even on adversarial input) when the file is
/// missing, oversized, truncated, checksum-mismatched, or has an unsupported
/// format version. Callers MUST fall back to rebuilding `PathIndex` from
/// segment doc tables on any `Err`: this sidecar is a pure performance cache,
/// never a source of truth.
pub(crate) fn read_paths_idx(dir: &Path) -> Result<PathIndex, SidecarError> {
    let path = dir.join(PATHS_IDX_FILENAME);
    // Check size via metadata before reading, so a huge/adversarial file
    // cannot force a huge allocation just to reject it.
    let meta = std::fs::metadata(&path).map_err(SidecarError::Io)?;
    if meta.len() > MAX_SIDECAR_SIZE {
        return Err(SidecarError::TooLarge(meta.len()));
    }
    let bytes = std::fs::read(&path).map_err(SidecarError::Io)?;
    decode(&bytes)
}

/// Decode and validate the full sidecar format (header + body), bounds-checked
/// throughout so a truncated or adversarial byte slice returns `Err` instead
/// of panicking.
fn decode(bytes: &[u8]) -> Result<PathIndex, SidecarError> {
    if bytes.len() < HEADER_LEN {
        return Err(SidecarError::TooShort);
    }
    if &bytes[0..4] != MAGIC {
        return Err(SidecarError::BadMagic);
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(SidecarError::UnsupportedVersion(version));
    }
    let checksum = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let body = &bytes[HEADER_LEN..];
    if xxh64(body, 0) != checksum {
        return Err(SidecarError::ChecksumMismatch);
    }

    let mut pos = 0usize;
    let path_count = read_u32(body, &mut pos)? as usize;
    let mut paths = Vec::with_capacity(path_count.min(MAX_PREALLOC_ENTRIES));
    for _ in 0..path_count {
        let len = read_u32(body, &mut pos)? as usize;
        let slice = read_bytes(body, &mut pos, len)?;
        paths.push(path_from_bytes(slice));
    }

    let extension_to_files = read_bitmap_table(body, &mut pos)?;
    let component_to_files = read_bitmap_table(body, &mut pos)?;
    if pos != body.len() {
        return Err(SidecarError::Truncated);
    }

    Ok(crate::path::from_sidecar_parts(
        paths,
        extension_to_files,
        component_to_files,
    ))
}

/// Read a little-endian `u32` at `*pos`, advancing `pos` by 4.
fn read_u32(body: &[u8], pos: &mut usize) -> Result<u32, SidecarError> {
    let bytes = body.get(*pos..*pos + 4).ok_or(SidecarError::Truncated)?;
    *pos += 4;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

/// Read `len` bytes at `*pos`, advancing `pos` by `len`.
///
/// Uses `checked_add` (not a plain `+`) because `len` comes from an untrusted
/// length prefix that can be up to `u32::MAX`; on a 32-bit `usize` target,
/// `*pos + len` could otherwise overflow and panic instead of returning the
/// `Truncated` error this function exists to produce.
fn read_bytes<'a>(body: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], SidecarError> {
    let end = pos.checked_add(len).ok_or(SidecarError::Truncated)?;
    let slice = body.get(*pos..end).ok_or(SidecarError::Truncated)?;
    *pos = end;
    Ok(slice)
}

/// Read a `(u32 key_len, key bytes, u32 bitmap_len, roaring bytes)*` table,
/// mirroring `write_bitmap_table`'s layout.
fn read_bitmap_table(
    body: &[u8],
    pos: &mut usize,
) -> Result<HashMap<Vec<u8>, RoaringBitmap>, SidecarError> {
    let count = read_u32(body, pos)? as usize;
    let mut table = HashMap::with_capacity(count.min(MAX_PREALLOC_ENTRIES));
    for _ in 0..count {
        let key_len = read_u32(body, pos)? as usize;
        let key = read_bytes(body, pos, key_len)?.to_vec();
        let bm_len = read_u32(body, pos)? as usize;
        let bm_bytes = read_bytes(body, pos, bm_len)?;
        let bitmap = roaring_util::deserialize(bm_bytes).map_err(SidecarError::Bitmap)?;
        table.insert(key, bitmap);
    }
    Ok(table)
}

/// Serialize `index`'s sorted path list and extension/component bitmaps.
fn encode(index: &PathIndex) -> Vec<u8> {
    for (i, path) in index.paths.iter().enumerate() {
        debug_assert_eq!(
            index.file_id(path),
            Some(i as u32),
            "PathIndex passed to paths_idx::encode must have positional file_ids"
        );
    }

    let mut body = Vec::with_capacity(1024 + index.paths.len() * 24);
    body.extend_from_slice(&(index.paths.len() as u32).to_le_bytes());
    for path in &index.paths {
        let bytes = path_bytes(path);
        body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&bytes);
    }
    write_bitmap_table(&mut body, &index.extension_to_files);
    write_bitmap_table(&mut body, &index.component_to_files);

    let checksum = xxh64(&body, 0);
    let mut out = Vec::with_capacity(4 + 4 + 8 + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn write_bitmap_table(body: &mut Vec<u8>, table: &HashMap<Vec<u8>, RoaringBitmap>) {
    // Sorted iteration order is not required for correctness (the decoder
    // rebuilds a HashMap), but makes the on-disk bytes deterministic across
    // writes of an identical `PathIndex`, which is a lot easier to debug and
    // to snapshot-test against.
    let mut entries: Vec<(&Vec<u8>, &RoaringBitmap)> = table.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));

    body.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, bitmap) in entries {
        body.extend_from_slice(&(key.len() as u32).to_le_bytes());
        body.extend_from_slice(key);
        let bm_bytes = roaring_util::serialize(bitmap);
        body.extend_from_slice(&(bm_bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(&bm_bytes);
    }
}

/// Atomically write `index` to `dir/paths.idx`.
///
/// Failure to write the sidecar is not fatal to the caller: `paths.idx` is a
/// pure performance cache, and `open()` falls back to rebuilding `PathIndex`
/// from segment doc tables whenever it is missing, corrupt, or stale. Callers
/// in `build.rs`/`compact.rs` log write failures when verbose rather than
/// aborting the build.
pub(crate) fn write_paths_idx(dir: &Path, index: &PathIndex) -> io::Result<()> {
    let bytes = encode(index);
    // Random tmp filename (matches `Manifest::save`) to prevent a TOCTOU
    // symlink attack where an attacker pre-creates a predictable
    // `paths.idx.tmp` path.
    let tmp = dir.join(format!("paths-{}.tmp", uuid::Uuid::new_v4()));
    let final_path = dir.join(PATHS_IDX_FILENAME);
    {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &final_path)?;
    #[cfg(not(windows))]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "paths_idx_tests.rs"]
mod tests;
