//! Where an index reads document bytes from.
//!
//! By default every read goes to the working tree under `Config::repo_root`,
//! guarded by the containment and TOCTOU checks at each read site. With the
//! `content-source` feature a caller can supply a [`ContentSource`] instead.
//! The index then reads the corpus and every document's bytes from it, never
//! reads the repository directory, and never spawns git. That is what lets an
//! index cover a revision that is not checked out: the caller resolves a path
//! at that revision to bytes, and syntext indexes and verifies those bytes.
//!
//! The default build carries only the [`DocReader::WorkingTree`] arm, so every
//! read site keeps its existing behavior byte for byte.

use std::path::Path;
#[cfg(feature = "content-source")]
use std::path::PathBuf;
#[cfg(feature = "content-source")]
use std::sync::Arc;

#[cfg(feature = "content-source")]
use crate::{Config, IndexError};

/// Supplies the corpus and document bytes for an index, in place of the
/// working tree.
///
/// Paths are repository-relative with forward slashes, the same form syntext
/// stores and returns in matches. `read` is called concurrently from search
/// threads, so implementations must be cheap to call in parallel.
#[cfg(feature = "content-source")]
pub trait ContentSource: Send + Sync {
    /// Every document a full build should index. Called once per
    /// [`Index::build_from_source`](super::Index::build_from_source).
    fn files(&self) -> std::io::Result<Vec<SourceFile>>;

    /// The bytes of `path`, or `Ok(None)` when the source has no such
    /// document. A document longer than `limit` bytes is treated as too large
    /// to index or verify, so an implementation may stop after `limit + 1`
    /// bytes.
    fn read(&self, path: &Path, limit: u64) -> std::io::Result<Option<Vec<u8>>>;
}

/// One document a [`ContentSource`] offers to a full build.
#[cfg(feature = "content-source")]
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceFile {
    /// Repository-relative path, stored in the index and returned in matches.
    pub path: PathBuf,
    /// Size in bytes, used for batching and the `max_file_size` filter.
    pub size_bytes: u64,
}

/// The reader an [`Index`](super::Index) uses for document bytes.
#[derive(Clone, Default)]
pub(crate) enum DocReader {
    /// The working tree under `Config::repo_root`.
    #[default]
    WorkingTree,
    /// A caller-supplied source.
    #[cfg(feature = "content-source")]
    Source(Arc<dyn ContentSource>),
}

/// Result of asking a [`DocReader`] for one document.
// Only `WorkingTree` is constructed without the feature; the other variants
// exist so read sites share one match shape across both builds.
#[cfg_attr(not(feature = "content-source"), allow(dead_code))]
pub(crate) enum SourceRead {
    /// No source is configured: read the working tree as before.
    WorkingTree,
    /// The source returned the document's bytes, within the size limit.
    Bytes(Vec<u8>),
    /// The source has no such document.
    Missing,
    /// The document is longer than the size limit.
    TooLarge,
    /// The source failed to read the document.
    Failed(std::io::Error),
}

impl DocReader {
    /// True when bytes come from the working tree, so git-driven freshness and
    /// on-disk checks apply.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn is_working_tree(&self) -> bool {
        matches!(self, DocReader::WorkingTree)
    }

    /// Read `path` from the source, enforcing `limit`. Returns
    /// [`SourceRead::WorkingTree`] when there is no source.
    pub(crate) fn read(&self, path: &Path, limit: u64) -> SourceRead {
        match self {
            DocReader::WorkingTree => {
                let _ = (path, limit);
                SourceRead::WorkingTree
            }
            #[cfg(feature = "content-source")]
            DocReader::Source(source) => match source.read(path, limit) {
                Ok(Some(raw)) if raw.len() as u64 > limit => SourceRead::TooLarge,
                Ok(Some(raw)) => SourceRead::Bytes(raw),
                Ok(None) => SourceRead::Missing,
                Err(e) => SourceRead::Failed(e),
            },
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl super::Index {
    /// Refuse an operation that reads the working tree or spawns git when this
    /// index reads from a content source.
    pub(super) fn require_working_tree(&self, operation: &str) -> Result<(), crate::IndexError> {
        if self.reader.is_working_tree() {
            return Ok(());
        }
        Err(crate::IndexError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("{operation} reads the working tree; this index uses a content source"),
        )))
    }

    /// Classify a changed path from the content source, or `None` to read
    /// the working tree. Mirrors the working-tree rules in `commit.rs`: a
    /// missing document is a deletion, an oversized or unreadable one is
    /// excluded, and binary content is excluded.
    pub(super) fn classify_from_source(
        &self,
        path: &Path,
    ) -> Option<super::commit::ChangedFileOutcome> {
        use super::commit::ChangedFileOutcome;
        let raw = match self.reader.read(path, self.config.max_file_size) {
            SourceRead::WorkingTree => return None,
            SourceRead::Bytes(raw) => raw,
            SourceRead::Missing => return Some(ChangedFileOutcome::Vanished),
            SourceRead::TooLarge => {
                log::debug!("skipping {}: exceeds max_file_size", path.display());
                return Some(ChangedFileOutcome::Excluded);
            }
            SourceRead::Failed(e) => {
                log::debug!("skipping {}: source read failed: {e}", path.display());
                return Some(ChangedFileOutcome::Excluded);
            }
        };
        let content = super::encoding::normalize_encoding(&raw);
        if super::walk::is_binary(&content) {
            return Some(ChangedFileOutcome::Excluded);
        }
        Some(ChangedFileOutcome::Indexed(std::sync::Arc::from(
            content.as_ref(),
        )))
    }
}

#[cfg(feature = "content-source")]
impl super::Index {
    /// Build a fresh index whose corpus and document bytes come from `source`.
    ///
    /// Behaves like [`Index::build`](super::Index::build) except that nothing
    /// is read from `config.repo_root` and git is never run. `repo_root` must
    /// still name an existing directory: it anchors the absolute paths that
    /// [`notify_change`](super::Index::notify_change) and
    /// [`notify_delete`](super::Index::notify_delete) accept. Documents larger
    /// than `config.max_file_size` are skipped, as are binary documents. The
    /// index-vs-scan threshold uses its default rather than timing reads.
    ///
    /// The returned index keeps reading from `source`: incremental commits,
    /// flushes, and search verification all go through it. Git-driven
    /// freshness (`update_from_git`, `rebuild_if_stale`) returns an
    /// `Unsupported` I/O error, and `search_fresh` degrades to `search`.
    ///
    /// Returns `PathOutsideRepo` for a path that is absolute or contains `..`,
    /// and an `InvalidInput` I/O error for a duplicate path.
    pub fn build_from_source(
        config: Config,
        source: Arc<dyn ContentSource>,
    ) -> Result<Self, IndexError> {
        let (file_list, skips) = source_file_list(&config, source.files()?)?;
        super::build::build_index_from_file_list(
            config,
            file_list,
            skips,
            super::build::BATCH_SIZE_BYTES,
            &DocReader::Source(source),
        )
    }

    /// Open an existing index and read document bytes from `source`.
    ///
    /// `source` must describe the same corpus the index was built or last
    /// flushed from. Verification re-reads bytes from it, so a source that
    /// has drifted can only cause missed matches, never invented ones. The
    /// same git-freshness limits as [`build_from_source`](Self::build_from_source)
    /// apply.
    pub fn open_with_source(
        config: Config,
        source: Arc<dyn ContentSource>,
    ) -> Result<Self, IndexError> {
        let mut index = Self::open(config)?;
        index.reader = DocReader::Source(source);
        Ok(index)
    }
}

/// Validate and order a source's corpus into the build's file-record form.
#[cfg(feature = "content-source")]
fn source_file_list(
    config: &Config,
    files: Vec<SourceFile>,
) -> Result<(Vec<super::walk::FileRecord>, super::walk::WalkSkips), IndexError> {
    let mut skips = super::walk::WalkSkips::default();
    let mut list = Vec::with_capacity(files.len());
    for file in files {
        if file.path.is_absolute() || crate::path_util::has_disallowed_component(&file.path) {
            return Err(IndexError::PathOutsideRepo(file.path));
        }
        if file.size_bytes > config.max_file_size {
            skips.too_large += 1;
            continue;
        }
        let rel = crate::path_util::normalize_to_forward_slashes(file.path);
        // The absolute path is never read in source mode; it only keeps the
        // shared record shape and log messages meaningful.
        list.push((config.repo_root.join(&rel), rel, file.size_bytes));
    }
    list.sort_unstable_by(|left, right| left.1.cmp(&right.1));
    if let Some(pair) = list.windows(2).find(|pair| pair[0].1 == pair[1].1) {
        return Err(IndexError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("content source lists {} twice", pair[0].1.display()),
        )));
    }
    Ok((list, skips))
}
