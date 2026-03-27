pub mod cli;
pub mod index;
pub mod path;
pub mod posting;
pub mod query;
pub mod search;
pub mod tokenizer;

use std::path::PathBuf;

/// Configuration for index building and searching.
#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum file size to index (bytes). Default: 10MB.
    pub max_file_size: u64,
    /// Maximum segments before triggering background merge. Default: 10.
    pub max_segments: usize,
    /// Path to index directory.
    pub index_dir: PathBuf,
    /// Repository root path.
    pub repo_root: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_file_size: 10 * 1024 * 1024,
            max_segments: 10,
            index_dir: PathBuf::from(".ripline"),
            repo_root: PathBuf::from("."),
        }
    }
}

/// A single line-level match returned by a search query.
#[derive(Debug, Clone)]
pub struct SearchMatch {
    /// Repository-relative path of the matching file.
    pub path: PathBuf,
    /// 1-based line number of the match within the file.
    pub line_number: u32,
    /// Full text of the matching line (without trailing newline).
    pub line_content: String,
    /// Byte offset of the start of the matching line from the beginning of the file.
    pub byte_offset: u64,
}

/// Search options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Glob pattern to restrict search to matching paths.
    pub path_filter: Option<String>,
    /// File type filter (e.g., "rs", "py").
    pub file_type: Option<String>,
    /// Maximum number of results.
    pub max_results: Option<usize>,
    /// Enable case-insensitive matching.
    pub case_insensitive: bool,
}

/// Counters and metadata reported by `Index::stats()`.
#[derive(Debug, Clone)]
pub struct IndexStats {
    /// Number of indexed files across all base segments.
    pub total_documents: usize,
    /// Number of on-disk RPLX segment files.
    pub total_segments: usize,
    /// Total distinct n-grams across all segments.
    pub total_grams: usize,
    /// Combined on-disk size of all segment files plus the manifest (bytes).
    pub index_size_bytes: u64,
    /// Git commit SHA the base segments were built against, if known.
    pub base_commit: Option<String>,
    /// Number of overlay generations since the last full rebuild.
    pub overlay_generations: usize,
    /// Number of dirty file edits buffered in the current overlay.
    pub pending_edits: usize,
}

/// Errors returned by index operations.
#[derive(Debug)]
pub enum IndexError {
    /// I/O error (file not found, permission denied, etc.)
    Io(std::io::Error),
    /// Invalid regex pattern.
    InvalidPattern(String),
    /// Index is corrupt and needs rebuilding.
    CorruptIndex(String),
    /// Path is outside the repository root.
    PathOutsideRepo(PathBuf),
    /// File exceeds maximum indexable size.
    FileTooLarge { path: PathBuf, size: u64 },
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
            IndexError::InvalidPattern(p) => write!(f, "invalid pattern: {p}"),
            IndexError::CorruptIndex(msg) => write!(f, "corrupt index: {msg}"),
            IndexError::PathOutsideRepo(p) => write!(f, "path outside repo: {}", p.display()),
            IndexError::FileTooLarge { path, size } => {
                write!(f, "file too large: {} ({size} bytes)", path.display())
            }
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
