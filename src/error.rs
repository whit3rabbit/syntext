//! Error types for syntext index operations.

use std::path::PathBuf;

/// Errors returned by index operations.
#[derive(Debug)]
#[must_use]
#[non_exhaustive]
pub enum IndexError {
    /// I/O error (file not found, permission denied, etc.)
    Io(std::io::Error),
    /// No index exists at the given index directory. Build one first
    /// (`Index::build`, or `st index` from the CLI).
    IndexNotFound(PathBuf),
    /// Invalid regex pattern.
    InvalidPattern(String),
    /// Index is corrupt and needs rebuilding.
    CorruptIndex(String),
    /// A query was too broad: materializing its posting lists would exceed the
    /// per-query memory budget. This is a defense against OOM from a crafted
    /// index or an overly-generic query on a large index; the index itself is
    /// healthy. Narrow the query (more-specific terms) and retry.
    QueryTooBroad {
        /// The per-query posting-byte limit that was exceeded.
        limit_bytes: usize,
    },
    /// Path is outside the repository root.
    PathOutsideRepo(PathBuf),
    /// File exceeds maximum indexable size.
    FileTooLarge {
        /// Path to the file.
        path: PathBuf,
        /// Size of the file in bytes.
        size: u64,
    },
    /// Another process holds a conflicting lock on the index directory.
    LockConflict(PathBuf),
    /// Overlay has grown too large relative to the base index.
    /// Call `Index::build()` to perform a full reindex.
    OverlayFull {
        /// Current number of overlay documents.
        overlay_docs: usize,
        /// Number of base documents at the time of the check.
        base_docs: usize,
    },
    /// Document ID space exceeded `u32::MAX`.
    DocIdOverflow {
        /// Number of base documents already allocated.
        base_doc_count: u32,
        /// Number of overlay documents requested on top of the base.
        overlay_docs: usize,
    },
}

impl From<std::io::Error> for IndexError {
    fn from(err: std::io::Error) -> Self {
        IndexError::Io(err)
    }
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Io(e) => write!(f, "I/O error: {e}"),
            IndexError::IndexNotFound(p) => {
                write!(
                    f,
                    "no index found at {}: run `st index` to build one",
                    p.display()
                )
            }
            IndexError::InvalidPattern(p) => write!(f, "invalid pattern: {p}"),
            IndexError::CorruptIndex(msg) => write!(f, "corrupt index: {msg}"),
            IndexError::QueryTooBroad { limit_bytes } => write!(
                f,
                "query too broad: would materialize more than {limit_bytes} bytes of postings; \
                 add more specific terms to narrow the search"
            ),
            IndexError::PathOutsideRepo(p) => {
                // Use only the last path component to avoid leaking absolute
                // filesystem layout in library/server contexts where this error
                // may be forwarded to an untrusted caller.
                let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                write!(f, "path outside repo: {name}")
            }
            IndexError::FileTooLarge { path, size } => {
                // Same rationale: show filename only, not the full absolute path.
                let name =
                    path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                write!(f, "file too large: {name} ({size} bytes)")
            }
            IndexError::LockConflict(p) => {
                write!(f, "index locked by another process: {}", p.display())
            }
            IndexError::OverlayFull { overlay_docs, base_docs } => write!(
                f,
                "overlay too large ({overlay_docs} overlay docs, {base_docs} base docs): \
                 run `st index` to rebuild"
            ),
            IndexError::DocIdOverflow {
                base_doc_count,
                overlay_docs,
            } => write!(
                f,
                "doc_id overflow: base {base_doc_count} docs plus {overlay_docs} overlay docs exceeds u32::MAX"
            ),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Io(e) => Some(e),
            _ => None,
        }
    }
}
