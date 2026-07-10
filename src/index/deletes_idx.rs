//! On-disk sidecar for the base-segment delete-set: `deletes-<uuid>.idx`.
//!
//! `Index::open` needs the set of base doc_ids that have been superseded
//! (modified files) or removed (deleted files) by durable incremental updates
//! (`index::delta`). Unlike `paths.idx`, this sidecar is a **source of truth,
//! not a cache**: a base doc is hidden from search results ONLY by
//! `IndexSnapshot::delete_set` (see `search/resolver.rs`), and the verifier
//! re-reads live file bytes for base docs. If the delete-set were lost, a
//! modified file's stale base doc AND its new delta-segment doc would both
//! match the live file, producing duplicate results. So callers MUST fail
//! CLOSED on any load error (return an error / force a rebuild), never fall
//! back to an empty set.
//!
//! The filename is generation-scoped (`deletes-<uuid>.idx`) and recorded in
//! `Manifest::overlay_deletes_file`. Each update writes a fresh file and points
//! the manifest at it; the previous file is GC'd after the manifest is saved.
//! Because the file is never overwritten in place, a crash between writing it
//! and saving the manifest cannot tear the delete-set the old manifest still
//! references.
//!
//! # Format (little-endian)
//!
//! ```text
//! magic:     4 bytes, b"STDL"
//! version:   u32
//! checksum:  u64            xxh64 of every byte that follows this field
//! bitmap:    roaring-serialized RoaringBitmap (portable format)
//! ```

use std::io;
use std::path::Path;

use roaring::RoaringBitmap;
use xxhash_rust::xxh64::xxh64;

use crate::posting::roaring_util;

const MAGIC: &[u8; 4] = b"STDL";
pub(crate) const FORMAT_VERSION: u32 = 1;

/// Fixed header size: 4-byte magic + u32 version + u64 checksum.
const HEADER_LEN: usize = 4 + 4 + 8;

/// Reject a sidecar bigger than this before allocating any decode buffers.
/// A 50M-doc index (the `MAX_TOTAL_DOCS` ceiling) fully deleted serializes to
/// well under this; the cap exists purely to bound a corrupt/adversarial file.
pub(crate) const MAX_SIDECAR_SIZE: u64 = 1024 * 1024 * 1024;

/// Reasons a `deletes-*.idx` sidecar was rejected. Every variant is fatal to
/// the caller: unlike `paths.idx`, there is no safe rebuild-from-nothing
/// fallback, so `open()` surfaces these as `CorruptIndex`.
#[derive(Debug)]
pub(crate) enum SidecarError {
    /// The file is missing, or a read/metadata syscall failed.
    Io(io::Error),
    /// On-disk size exceeds `MAX_SIDECAR_SIZE`, checked before any read.
    TooLarge(u64),
    /// Fewer than `HEADER_LEN` bytes: not even a full header.
    TooShort,
    /// First 4 bytes are not `b"STDL"`.
    BadMagic,
    /// Version field does not match `FORMAT_VERSION`.
    UnsupportedVersion(u32),
    /// xxh64 over the body does not match the stored checksum.
    ChecksumMismatch,
    /// The generation-scoped filename recorded in the manifest is not a plain
    /// filename (contains a path separator, `..`, or is absolute).
    BadFilename,
    /// The roaring bitmap payload failed to deserialize.
    Bitmap(String),
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidecarError::Io(e) => write!(f, "I/O error reading deletes.idx: {e}"),
            SidecarError::TooLarge(n) => write!(
                f,
                "deletes.idx is {n} bytes, exceeds {MAX_SIDECAR_SIZE}-byte safety cap"
            ),
            SidecarError::TooShort => write!(f, "deletes.idx is shorter than its fixed header"),
            SidecarError::BadMagic => write!(f, "deletes.idx has an invalid magic number"),
            SidecarError::UnsupportedVersion(v) => write!(
                f,
                "deletes.idx format version {v} is not supported (expected {FORMAT_VERSION})"
            ),
            SidecarError::ChecksumMismatch => {
                write!(f, "deletes.idx checksum does not match its contents")
            }
            SidecarError::BadFilename => {
                write!(
                    f,
                    "deletes.idx filename in manifest is not a plain filename"
                )
            }
            SidecarError::Bitmap(e) => write!(f, "deletes.idx has a corrupt roaring bitmap: {e}"),
        }
    }
}

/// Reject a manifest-supplied sidecar filename that could escape the index dir.
/// Mirrors the segment-filename validation in `open.rs`.
fn is_plain_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !Path::new(name).is_absolute()
}

/// Generate a fresh generation-scoped sidecar filename.
pub(crate) fn new_filename() -> String {
    format!("deletes-{}.idx", uuid::Uuid::new_v4())
}

/// Load and validate `dir/name`, returning the decoded delete-set.
///
/// `name` is the generation-scoped filename recorded in
/// `Manifest::overlay_deletes_file`; it is validated as a plain filename before
/// being joined onto `dir`. Any `Err` MUST be treated as fatal by the caller
/// (fail closed) -- see the module docs.
pub(crate) fn read_deletes_idx(dir: &Path, name: &str) -> Result<RoaringBitmap, SidecarError> {
    if !is_plain_filename(name) {
        return Err(SidecarError::BadFilename);
    }
    let path = dir.join(name);
    // Check size via metadata before reading, so a huge/adversarial file cannot
    // force a huge allocation just to reject it.
    let meta = std::fs::metadata(&path).map_err(SidecarError::Io)?;
    if meta.len() > MAX_SIDECAR_SIZE {
        return Err(SidecarError::TooLarge(meta.len()));
    }
    let bytes = std::fs::read(&path).map_err(SidecarError::Io)?;
    decode(&bytes)
}

/// Decode and validate the full sidecar format (header + body), bounds-checked
/// so a truncated or adversarial byte slice returns `Err` instead of panicking.
fn decode(bytes: &[u8]) -> Result<RoaringBitmap, SidecarError> {
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
    roaring_util::deserialize(body).map_err(SidecarError::Bitmap)
}

/// Serialize `bitmap` into the sidecar byte layout.
fn encode(bitmap: &RoaringBitmap) -> Vec<u8> {
    let body = roaring_util::serialize(bitmap);
    let checksum = xxh64(&body, 0);
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Atomically write `bitmap` to `dir/name`.
///
/// Uses a random-uuid tmp file + fsync + rename (matches `Manifest::save` /
/// `write_paths_idx`) so a reader never sees a partial file and a TOCTOU
/// symlink attack on a predictable tmp path is not possible. Unlike
/// `paths.idx`, a write failure here IS fatal to the calling update (the
/// delete-set is a source of truth), so callers propagate the error rather
/// than logging and continuing.
pub(crate) fn write_deletes_idx(dir: &Path, name: &str, bitmap: &RoaringBitmap) -> io::Result<()> {
    if !is_plain_filename(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "deletes.idx filename is not a plain filename",
        ));
    }
    let bytes = encode(bitmap);
    let tmp = dir.join(format!("deletes-{}.tmp", uuid::Uuid::new_v4()));
    let final_path = dir.join(name);
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
#[path = "deletes_idx_tests.rs"]
mod tests;
