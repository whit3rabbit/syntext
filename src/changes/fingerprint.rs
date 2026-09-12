//! Content digest and file fingerprinting using BLAKE3.
//!
//! Separates cheap metadata inspection (`size`, `mtime`) from cryptographic
//! content hashing (`ContentDigest`). Integrates the racy-mtime settlement rule
//! to ensure that files written within coarse filesystem timestamp ticks are
//! identified safely.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How far an mtime must precede a content read to be considered settled.
/// Matches git's racily-clean margin (coarse timestamps on HFS+/FAT/NFS).
pub const RACY_MARGIN: Duration = Duration::from_secs(2);

/// Domain separation string prefix for file content digests.
const FILE_DIGEST_DOMAIN: &[u8] = b"file-v1\n";

/// 32-byte BLAKE3 content digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// Create a digest from a 32-byte array.
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Access the underlying 32-byte array.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compute digest over bytes in memory with domain separation prefix.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(FILE_DIGEST_DOMAIN);
        hasher.update(bytes);
        Self(*hasher.finalize().as_bytes())
    }

    /// Stream-hash an arbitrary reader with domain separation prefix using a
    /// 64 KiB buffer to keep memory overhead bounded.
    pub fn from_reader(reader: &mut dyn Read) -> io::Result<Self> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(FILE_DIGEST_DOMAIN);
        let mut buf = [0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    hasher.update(&buf[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    /// Parse a 64-character lowercase or uppercase hex string.
    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn from_hex(hex_str: &str) -> Result<Self, hex::FromHexError> {
        if hex_str.len() != 64 {
            return Err(hex::FromHexError::InvalidStringLength);
        }
        let mut out = [0u8; 32];
        for (i, chunk) in hex_str.as_bytes().chunks_exact(2).enumerate() {
            let val = hex_byte(chunk[0], chunk[1])?;
            out[i] = val;
        }
        Ok(Self(out))
    }

    /// Render lowercase hex representation.
    pub fn to_hex(&self) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for &byte in &self.0 {
            s.push(HEX_CHARS[(byte >> 4) as usize] as char);
            s.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
        }
        s
    }
}

fn hex_byte(hi: u8, lo: u8) -> Result<u8, hex::FromHexError> {
    let h = hex_nibble(hi)?;
    let l = hex_nibble(lo)?;
    Ok((h << 4) | l)
}

fn hex_nibble(b: u8) -> Result<u8, hex::FromHexError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(hex::FromHexError::InvalidHexCharacter {
            c: b as char,
            index: 0,
        }),
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentDigest({})", &self.to_hex()[..12])
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Settled or observed file fingerprint combining content hash with stat metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileFingerprint {
    /// Cryptographic BLAKE3 digest of the file bytes.
    pub digest: ContentDigest,
    /// Byte size from file metadata.
    pub size: u64,
    /// Seconds since UNIX epoch for mtime.
    pub mtime_secs: i64,
    /// Nanoseconds fraction for mtime.
    pub mtime_nanos: u32,
}

impl FileFingerprint {
    /// Create a new fingerprint.
    pub fn new(digest: ContentDigest, size: u64, mtime_secs: i64, mtime_nanos: u32) -> Self {
        Self {
            digest,
            size,
            mtime_secs,
            mtime_nanos,
        }
    }

    /// Check whether this observation is settled enough to trust that subsequent
    /// writes will not share the identical mtime.
    pub fn is_settled(&self, read_epoch: SystemTime, now: SystemTime) -> bool {
        let Some(mtime) = join_system_time(self.mtime_secs, self.mtime_nanos) else {
            return false;
        };
        if mtime > now {
            return false;
        }
        mtime + RACY_MARGIN < read_epoch
    }
}

/// Metadata observed via `stat` without reading file contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatObservation {
    /// File is absent or was removed.
    Absent,
    /// File is present with known size and timestamp.
    Present {
        size: u64,
        mtime_secs: i64,
        mtime_nanos: u32,
    },
}

impl StatObservation {
    /// Observe path metadata via `symlink_metadata`.
    pub fn stat(abs_path: &Path) -> Option<Self> {
        match std::fs::symlink_metadata(abs_path) {
            Ok(meta) => {
                if !meta.is_file() {
                    return None;
                }
                let modified = meta.modified().ok()?;
                let (secs, nanos) = split_system_time(modified)?;
                Some(StatObservation::Present {
                    size: meta.len(),
                    mtime_secs: secs,
                    mtime_nanos: nanos,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(StatObservation::Absent),
            Err(_) => None,
        }
    }

    /// Check if this stat matches an existing fingerprint in size and mtime.
    pub fn matches_fingerprint(&self, fp: &FileFingerprint) -> bool {
        match self {
            StatObservation::Present {
                size,
                mtime_secs,
                mtime_nanos,
            } => *size == fp.size && *mtime_secs == fp.mtime_secs && *mtime_nanos == fp.mtime_nanos,
            StatObservation::Absent => false,
        }
    }
}

/// Result of hashing a file, optionally retaining pre-read content buffer.
pub struct HashedContent {
    /// Computed fingerprint.
    pub fingerprint: FileFingerprint,
    /// Bounded in-memory buffer if the file was smaller than the retention limit.
    pub buffer: Option<Arc<[u8]>>,
}

/// Read and compute the fingerprint of `abs_path`.
/// If `size <= max_retain_bytes`, the content bytes are read into an `Arc<[u8]>`
/// and returned alongside the fingerprint to eliminate downstream secondary reads.
pub fn hash_file(abs_path: &Path, max_retain_bytes: usize) -> io::Result<HashedContent> {
    let mut file = File::open(abs_path)?;
    let meta = file.metadata()?;
    let size = meta.len();
    let (mtime_secs, mtime_nanos) = meta
        .modified()
        .ok()
        .and_then(split_system_time)
        .unwrap_or((0, 0));

    if size <= max_retain_bytes as u64 {
        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf)?;
        let digest = ContentDigest::from_bytes(&buf);
        let arc: Arc<[u8]> = Arc::from(buf);
        Ok(HashedContent {
            fingerprint: FileFingerprint::new(digest, size, mtime_secs, mtime_nanos),
            buffer: Some(arc),
        })
    } else {
        let digest = ContentDigest::from_reader(&mut file)?;
        Ok(HashedContent {
            fingerprint: FileFingerprint::new(digest, size, mtime_secs, mtime_nanos),
            buffer: None,
        })
    }
}

pub(crate) fn split_system_time(t: SystemTime) -> Option<(i64, u32)> {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => Some((i64::try_from(d.as_secs()).ok()?, d.subsec_nanos())),
        Err(_) => None,
    }
}

pub(crate) fn join_system_time(secs: i64, nanos: u32) -> Option<SystemTime> {
    let secs = u64::try_from(secs).ok()?;
    UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

pub mod hex {
    use std::fmt;

    #[derive(Debug, PartialEq, Eq)]
    pub enum FromHexError {
        InvalidHexCharacter { c: char, index: usize },
        InvalidStringLength,
    }

    impl fmt::Display for FromHexError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                FromHexError::InvalidHexCharacter { c, .. } => {
                    write!(f, "invalid hex character: {c}")
                }
                FromHexError::InvalidStringLength => write!(f, "invalid hex length"),
            }
        }
    }

    impl std::error::Error for FromHexError {}
}

#[cfg(test)]
#[path = "fingerprint_tests.rs"]
mod tests;
