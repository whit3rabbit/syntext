//! Change batch and record structures representing observed repository drift.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::fingerprint::{ContentDigest, FileFingerprint};

/// Classification of an observed path in a change batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    /// Newly observed path that was not previously in the catalogue.
    Added,
    /// Path whose content digest has changed.
    Modified,
    /// Path whose mtime or size shifted, but cryptographic content digest is identical.
    /// Downstream consumers (lexical index, AST parser, embedder) can safely skip work.
    Touched,
    /// Path that previously existed in the catalogue but is now absent on disk.
    Deleted,
    /// Path whose settled stat metadata is unchanged since the last observation.
    Unchanged,
    /// Path could not be observed reliably (I/O error, permissions, interrupted scan).
    Uncertain,
}

impl ChangeKind {
    /// True if the indexed content bytes changed (Added or Modified).
    pub fn is_content_changed(self) -> bool {
        matches!(self, ChangeKind::Added | ChangeKind::Modified)
    }

    /// True if the path represents a removal.
    pub fn is_deleted(self) -> bool {
        matches!(self, ChangeKind::Deleted)
    }
}

/// Record of an observation at a single repository-relative path.
#[derive(Clone, PartialEq, Eq)]
pub struct ChangeRecord {
    /// Repository-relative normalized path.
    pub path: PathBuf,
    /// Kind of change observed.
    pub kind: ChangeKind,
    /// Current fingerprint (set for Added, Modified, Touched, Unchanged).
    pub current_fingerprint: Option<FileFingerprint>,
    /// Previous fingerprint from catalogue before this observation.
    pub previous_fingerprint: Option<FileFingerprint>,
    /// Pre-read content bytes if within the retention size cap.
    pub content: Option<Arc<[u8]>>,
}

impl std::fmt::Debug for ChangeRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeRecord")
            .field("path", &self.path)
            .field("kind", &self.kind)
            .field("current_fingerprint", &self.current_fingerprint)
            .field("previous_fingerprint", &self.previous_fingerprint)
            .field("has_content", &self.content.is_some())
            .finish()
    }
}

impl ChangeRecord {
    /// Path of this record.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current content digest if present.
    pub fn digest(&self) -> Option<&ContentDigest> {
        self.current_fingerprint.as_ref().map(|fp| &fp.digest)
    }

    /// True if content changed (requires re-indexing / re-embedding).
    pub fn is_content_changed(&self) -> bool {
        self.kind.is_content_changed()
    }
}

/// A generation-tagged batch of observed changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeBatch {
    /// Generation identifier assigned to this batch.
    pub generation: u64,
    /// Observed records in this batch.
    pub records: Vec<ChangeRecord>,
    /// True if any paths had uncertainty (e.g. permission error, lost event).
    pub has_uncertainty: bool,
}

impl ChangeBatch {
    /// Create a new batch.
    pub fn new(generation: u64, records: Vec<ChangeRecord>, has_uncertainty: bool) -> Self {
        Self {
            generation,
            records,
            has_uncertainty,
        }
    }

    /// Number of records in the batch.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// True if the batch contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Iterator over records whose content bytes changed (`Added` or `Modified`).
    pub fn content_changes(&self) -> impl Iterator<Item = &ChangeRecord> {
        self.records.iter().filter(|r| r.is_content_changed())
    }

    /// Iterator over records that were deleted.
    pub fn deletions(&self) -> impl Iterator<Item = &ChangeRecord> {
        self.records.iter().filter(|r| r.kind.is_deleted())
    }
}
