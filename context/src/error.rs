//! Error type for context-engine operations.

use std::path::PathBuf;

/// Convenience alias used throughout the crate.
pub type Result<T, E = ContextError> = std::result::Result<T, E>;

/// Errors returned by the context engine.
#[derive(Debug)]
#[must_use]
#[non_exhaustive]
pub enum ContextError {
    /// Underlying I/O failure.
    Io(std::io::Error),
    /// A store file failed validation (bad magic, checksum, bounds, or
    /// structure). The store needs `compact`, a re-sync, or a rebuild.
    Corrupt(String),
    /// A store file was written by an incompatible format version.
    UnsupportedVersion {
        /// Version found on disk.
        found: u32,
        /// Highest version this build reads.
        supported: u32,
    },
    /// Another handle holds the store's write lock.
    Locked(PathBuf),
    /// The handle was opened read-only and a write was attempted.
    ReadOnly,
    /// No store exists at the given directory.
    NotFound(PathBuf),
    /// A caller-supplied value was rejected (bad key, scope, or option).
    InvalidInput(String),
    /// An error surfaced by the underlying syntext index.
    Index(syntext::IndexError),
    /// The system prompt and question alone, with no history and no
    /// evidence, do not fit the model window.
    WindowExceeded {
        /// Tokens the smallest possible prompt needs.
        needed: usize,
        /// Tokens available after the answer reserve.
        limit: usize,
    },
    /// The caller cancelled the operation (for example from a sync
    /// progress callback). Nothing was published.
    Cancelled,
}

impl ContextError {
    /// Build a [`ContextError::Corrupt`] from anything displayable.
    pub(crate) fn corrupt(msg: impl Into<String>) -> Self {
        ContextError::Corrupt(msg.into())
    }

    /// Build a [`ContextError::InvalidInput`] from anything displayable.
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        ContextError::InvalidInput(msg.into())
    }
}

impl From<std::io::Error> for ContextError {
    fn from(err: std::io::Error) -> Self {
        ContextError::Io(err)
    }
}

impl From<syntext::IndexError> for ContextError {
    fn from(err: syntext::IndexError) -> Self {
        ContextError::Index(err)
    }
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextError::Io(e) => write!(f, "I/O error: {e}"),
            ContextError::Corrupt(msg) => write!(f, "corrupt context store: {msg}"),
            ContextError::UnsupportedVersion { found, supported } => write!(
                f,
                "unsupported context store format version {found} (this build reads up to {supported})"
            ),
            ContextError::Locked(p) => {
                write!(f, "context store is locked by another writer: {}", p.display())
            }
            ContextError::ReadOnly => write!(f, "context store handle is read-only"),
            ContextError::NotFound(p) => write!(f, "no context store at {}", p.display()),
            ContextError::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            ContextError::Index(e) => write!(f, "syntext index error: {e}"),
            ContextError::WindowExceeded { needed, limit } => write!(
                f,
                "prompt needs {needed} tokens even without history or evidence, but only {limit} fit the window"
            ),
            ContextError::Cancelled => write!(f, "operation cancelled"),
        }
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ContextError::Io(e) => Some(e),
            ContextError::Index(e) => Some(e),
            _ => None,
        }
    }
}
