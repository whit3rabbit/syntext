//! On-disk format for the working-tree anchor: `worktree-<uuid>.idx`.
//!
//! Split from [`super::worktree_anchor`] (which owns the semantics) to keep
//! both files under the 400-line quality gate.
//!
//! # Format (little-endian)
//!
//! ```text
//! magic:     4 bytes, b"STWT"
//! version:   u32
//! checksum:  u64            xxh64 of every byte that follows this field
//! count:     u32
//! entries:   count * {
//!     path_len:    u32
//!     path:        path_len bytes (repo-relative, forward slashes)
//!     kind:        u8     0 = Absent, 1 = Present
//!     size:        u64    0 when Absent
//!     mtime_secs:  i64    0 when Absent
//!     mtime_nanos: u32    0 when Absent
//! }
//! ```
//!
//! A read error is answered by falling back to an empty anchor, never by
//! refusing to open the index. See the fail-open discussion in
//! [`super::worktree_anchor`].

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use xxhash_rust::xxh64::xxh64;

use super::worktree_anchor::{Observed, WorktreeAnchor};
use crate::path_util::{path_bytes, path_from_bytes};

const MAGIC: &[u8; 4] = b"STWT";
pub(crate) const FORMAT_VERSION: u32 = 1;

/// Fixed header: 4-byte magic + u32 version + u64 checksum.
const HEADER_LEN: usize = 4 + 4 + 8;

/// Reject a sidecar bigger than this before allocating any decode buffer.
const MAX_SIDECAR_SIZE: u64 = 64 * 1024 * 1024;

/// Bound on the per-entry allocation a corrupt `count` can request.
const MAX_PREALLOC_ENTRIES: usize = 4096;

/// Generate a fresh generation-scoped sidecar filename.
pub(crate) fn new_filename() -> String {
    format!("worktree-{}.idx", uuid::Uuid::new_v4())
}

/// Load `dir/name`.
///
/// Callers fail OPEN on `Err` (log and use an empty anchor). See the module
/// docs for why that is safe here and not in `deletes_idx`.
pub(crate) fn read_worktree_anchor(dir: &Path, name: &str) -> Result<WorktreeAnchor, SidecarError> {
    if !super::deletes_idx::is_plain_filename(name) {
        return Err(SidecarError::BadFilename);
    }
    let path = dir.join(name);
    let meta = std::fs::metadata(&path).map_err(SidecarError::Io)?;
    if meta.len() > MAX_SIDECAR_SIZE {
        return Err(SidecarError::TooLarge(meta.len()));
    }
    let bytes = std::fs::read(&path).map_err(SidecarError::Io)?;
    decode(&bytes)
}

/// Errors from reading a `worktree-*.idx`. Every variant is answered by
/// falling back to an empty anchor, never by refusing to open the index.
#[derive(Debug)]
pub(crate) enum SidecarError {
    Io(io::Error),
    TooLarge(u64),
    Malformed(&'static str),
    UnsupportedVersion(u32),
    ChecksumMismatch,
    BadFilename,
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SidecarError::Io(e) => write!(f, "I/O error reading worktree anchor: {e}"),
            SidecarError::TooLarge(n) => write!(
                f,
                "worktree anchor is {n} bytes, exceeds {MAX_SIDECAR_SIZE}-byte safety cap"
            ),
            SidecarError::Malformed(what) => write!(f, "worktree anchor is malformed: {what}"),
            SidecarError::UnsupportedVersion(v) => write!(
                f,
                "worktree anchor format version {v} is not supported (expected {FORMAT_VERSION})"
            ),
            SidecarError::ChecksumMismatch => {
                write!(f, "worktree anchor checksum does not match its contents")
            }
            SidecarError::BadFilename => {
                write!(f, "worktree anchor filename in manifest is not a plain filename")
            }
        }
    }
}

fn decode(bytes: &[u8]) -> Result<WorktreeAnchor, SidecarError> {
    if bytes.len() < HEADER_LEN {
        return Err(SidecarError::Malformed("shorter than its fixed header"));
    }
    if &bytes[0..4] != MAGIC {
        return Err(SidecarError::Malformed("invalid magic number"));
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
    let count = read_u32(body, &mut pos)? as usize;
    let mut entries = HashMap::with_capacity(count.min(MAX_PREALLOC_ENTRIES));
    for _ in 0..count {
        let path_len = read_u32(body, &mut pos)? as usize;
        let end = pos
            .checked_add(path_len)
            .ok_or(SidecarError::Malformed("path length overflow"))?;
        if end > body.len() {
            return Err(SidecarError::Malformed("truncated path"));
        }
        let rel = path_from_bytes(&body[pos..end]);
        pos = end;
        let kind = read_u8(body, &mut pos)?;
        let size = read_u64(body, &mut pos)?;
        let mtime_secs = read_u64(body, &mut pos)? as i64;
        let mtime_nanos = read_u32(body, &mut pos)?;
        let observed = match kind {
            0 => Observed::Absent,
            1 => Observed::Present {
                size,
                mtime_secs,
                mtime_nanos,
            },
            _ => return Err(SidecarError::Malformed("unknown entry kind")),
        };
        entries.insert(rel, observed);
    }
    if pos != body.len() {
        return Err(SidecarError::Malformed("trailing bytes after entries"));
    }
    Ok(WorktreeAnchor::from_entries(entries))
}

fn read_u8(body: &[u8], pos: &mut usize) -> Result<u8, SidecarError> {
    let byte = *body
        .get(*pos)
        .ok_or(SidecarError::Malformed("truncated entry"))?;
    *pos += 1;
    Ok(byte)
}

fn read_u32(body: &[u8], pos: &mut usize) -> Result<u32, SidecarError> {
    let end = pos
        .checked_add(4)
        .ok_or(SidecarError::Malformed("offset overflow"))?;
    let slice = body
        .get(*pos..end)
        .ok_or(SidecarError::Malformed("truncated entry"))?;
    *pos = end;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u64(body: &[u8], pos: &mut usize) -> Result<u64, SidecarError> {
    let end = pos
        .checked_add(8)
        .ok_or(SidecarError::Malformed("offset overflow"))?;
    let slice = body
        .get(*pos..end)
        .ok_or(SidecarError::Malformed("truncated entry"))?;
    *pos = end;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn encode(anchor: &WorktreeAnchor) -> Vec<u8> {
    // Sorted so the bytes (and therefore the checksum) are reproducible for a
    // given anchor, which is what makes the round-trip tests meaningful.
    let mut sorted: Vec<(&PathBuf, &Observed)> = anchor.entries.iter().collect();
    sorted.sort_unstable_by(|a, b| a.0.cmp(b.0));

    let mut body = Vec::with_capacity(4 + sorted.len() * 48);
    body.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    for (rel, observed) in sorted {
        let bytes = path_bytes(rel);
        body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(bytes.as_ref());
        match observed {
            Observed::Absent => {
                body.push(0);
                body.extend_from_slice(&0u64.to_le_bytes());
                body.extend_from_slice(&0u64.to_le_bytes());
                body.extend_from_slice(&0u32.to_le_bytes());
            }
            Observed::Present {
                size,
                mtime_secs,
                mtime_nanos,
            } => {
                body.push(1);
                body.extend_from_slice(&size.to_le_bytes());
                body.extend_from_slice(&(*mtime_secs as u64).to_le_bytes());
                body.extend_from_slice(&mtime_nanos.to_le_bytes());
            }
        }
    }

    let checksum = xxh64(&body, 0);
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Atomically write `anchor` to `dir/name` (tmp + fsync + rename), the same way
/// `deletes_idx` and `paths.idx` are written.
pub(crate) fn write_worktree_anchor(
    dir: &Path,
    name: &str,
    anchor: &WorktreeAnchor,
) -> io::Result<()> {
    if !super::deletes_idx::is_plain_filename(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree anchor filename is not a plain filename",
        ));
    }
    let bytes = encode(anchor);
    let tmp = dir.join(format!("worktree-{}.tmp", uuid::Uuid::new_v4()));
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
#[path = "worktree_codec_tests.rs"]
mod tests;
