//! Overlay: in-memory view of dirty files for incremental updates.
//!
//! The overlay provides read-your-writes freshness with atomic batch commits
//! and snapshot isolation. Pending edits are invisible until `commit_batch()`.
//!
//! Design: single merged query view (research.md section 7). Each
//! `commit_batch()` incrementally rebuilds the overlay, reusing docs from
//! the previous generation for unchanged files and reading only the delta
//! from disk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tokenizer::build_all;
use crate::IndexError;

/// Kind of file change buffered by `notify_change` / `notify_delete`.
#[derive(Debug, Clone)]
pub enum EditKind {
    /// File was added or modified (content changed).
    Changed,
    /// File was removed from the repository.
    Deleted,
}

/// A buffered file edit not yet committed to the index snapshot.
#[derive(Debug, Clone)]
pub struct FileEdit {
    /// Repository-relative path of the changed file.
    pub path: PathBuf,
    /// Nature of the change.
    pub kind: EditKind,
}

/// A dirty file tracked by the overlay with its current content and grams.
#[derive(Debug, Clone)]
pub struct OverlayDoc {
    /// Overlay-space doc_id (disjoint from base segment range).
    pub doc_id: u32,
    /// Repository-relative path.
    pub path: PathBuf,
    /// Current file content (kept for verification during search).
    /// Arc-shared to avoid cloning between snapshot generations.
    pub content: Arc<[u8]>,
    /// Cached gram hashes for this document. Avoids re-tokenization
    /// when the doc is carried forward to the next overlay generation.
    pub grams: Vec<u64>,
}

/// Single merged in-memory gram index for all dirty files.
///
/// Rebuilt from scratch on each `commit_batch()`. Query execution always
/// does two lookups: base segments + this single overlay.
#[derive(Clone)]
pub struct OverlayView {
    /// Map from gram hash to sorted overlay doc_ids that contain it.
    pub gram_index: HashMap<u64, Vec<u32>>,
    /// All dirty files with current content.
    pub docs: Vec<OverlayDoc>,
    /// doc_id -> index into `docs` for O(1) lookup.
    doc_id_map: HashMap<u32, usize>,
    /// Next overlay-space doc_id (starts after base range).
    pub next_doc_id: u32,
    /// The base_doc_count at which this overlay was built.
    /// Used to detect whether a segment flush occurred between commits.
    pub base_doc_count: u32,
}

impl OverlayView {
    /// Create an empty overlay view.
    pub fn empty() -> Self {
        OverlayView {
            gram_index: HashMap::new(),
            docs: Vec::new(),
            doc_id_map: HashMap::new(),
            next_doc_id: 0,
            base_doc_count: 0,
        }
    }

    /// Build an overlay from a set of dirty files.
    ///
    /// # Caller contract: content must be pre-normalized
    ///
    /// `dirty_files` content is stored as-is and returned by the resolver for
    /// verification. Callers MUST call `normalize_encoding` before building the
    /// overlay so that UTF-16 / BOM content is transcoded to UTF-8. `commit_batch`
    /// in `index/mod.rs` satisfies this contract. Direct callers (e.g., tests)
    /// using raw byte literals are exempt because ASCII source is already valid UTF-8.
    ///
    /// `base_doc_count` is the total doc count across all base segments.
    /// Overlay doc_ids start at `base_doc_count` to stay disjoint.
    /// `dirty_files` maps repo-relative path to file content.
    pub fn build(
        base_doc_count: u32,
        dirty_files: Vec<(PathBuf, Arc<[u8]>)>,
    ) -> Result<Self, IndexError> {
        let mut gram_index: HashMap<u64, Vec<u32>> = HashMap::new();
        let overlay_docs = dirty_files.len();
        let mut docs = Vec::with_capacity(dirty_files.len());
        let mut next_id = base_doc_count;

        for (path, content) in dirty_files {
            let doc_id = Self::next_doc_id(&mut next_id, base_doc_count, overlay_docs)?;

            let grams = build_all(&content);
            for &gram_hash in &grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }

            docs.push(OverlayDoc {
                doc_id,
                path,
                content,
                grams,
            });
        }

        // Sort posting lists for consistent intersection behavior.
        for ids in gram_index.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }

        let doc_id_map = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.doc_id, i))
            .collect();

        Ok(OverlayView {
            gram_index,
            docs,
            doc_id_map,
            next_doc_id: next_id,
            base_doc_count,
        })
    }

    /// Build an overlay incrementally, reusing docs from the previous overlay
    /// for files that haven't changed since the last commit.
    ///
    /// Only `new_files` are read from disk and tokenized fresh. Unchanged docs
    /// from `old_overlay` are carried forward with `Arc::clone` on content
    /// (refcount bump, no data copy). All docs are re-indexed with new doc_ids
    /// since the ID space starts at `base_doc_count`.
    ///
    /// Fast path: when `base_doc_count == old_overlay.base_doc_count`, overlay
    /// doc_ids for unchanged files are stable and the delta path is used instead
    /// of a full rebuild.
    pub fn build_incremental(
        base_doc_count: u32,
        old_overlay: &OverlayView,
        new_files: Vec<(PathBuf, Arc<[u8]>)>,
        newly_changed: &HashSet<PathBuf>,
        removed_paths: &HashSet<PathBuf>,
    ) -> Result<Self, IndexError> {
        // Fast path: base has not grown since the last commit.
        // Overlay doc_ids for unchanged files are stable; use delta update.
        if base_doc_count == old_overlay.base_doc_count {
            return Self::build_incremental_delta(
                base_doc_count,
                old_overlay,
                new_files,
                newly_changed,
                removed_paths,
            );
        }

        let mut gram_index: HashMap<u64, Vec<u32>> = HashMap::new();
        let overlay_docs =
            (old_overlay.docs.len() + new_files.len()).saturating_sub(newly_changed.len());
        let mut docs = Vec::new();
        let mut next_id = base_doc_count;

        // Reuse old overlay docs that are unchanged.
        for old_doc in &old_overlay.docs {
            if removed_paths.contains(&old_doc.path) {
                continue;
            }
            if newly_changed.contains(&old_doc.path) {
                continue; // replaced by new version below
            }
            let doc_id = Self::next_doc_id(&mut next_id, base_doc_count, overlay_docs)?;

            // Reuse cached grams instead of re-tokenizing.
            for &gram_hash in &old_doc.grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }
            docs.push(OverlayDoc {
                doc_id,
                path: old_doc.path.clone(),
                content: Arc::clone(&old_doc.content),
                grams: old_doc.grams.clone(),
            });
        }

        // Add newly changed/added files (freshly read from disk).
        for (path, content) in new_files {
            let doc_id = Self::next_doc_id(&mut next_id, base_doc_count, overlay_docs)?;

            let grams = build_all(&content);
            for &gram_hash in &grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }
            docs.push(OverlayDoc {
                doc_id,
                path,
                content,
                grams,
            });
        }

        for ids in gram_index.values_mut() {
            ids.sort_unstable();
            ids.dedup();
        }

        let doc_id_map = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.doc_id, i))
            .collect();

        Ok(OverlayView {
            gram_index,
            docs,
            doc_id_map,
            next_doc_id: next_id,
            base_doc_count,
        })
    }

    /// Allocate the next overlay doc_id, advancing `next_id` by 1.
    #[inline]
    fn next_doc_id(
        next_id: &mut u32,
        base_doc_count: u32,
        overlay_docs: usize,
    ) -> Result<u32, IndexError> {
        let id = *next_id;
        *next_id = next_id.checked_add(1).ok_or(IndexError::DocIdOverflow {
            base_doc_count,
            overlay_docs,
        })?;
        Ok(id)
    }

    /// Delta path: base_doc_count is unchanged, so overlay doc_ids for unchanged
    /// files are stable. Clone the old gram_index, remove stale entries for
    /// changed/deleted files using their cached grams, append new doc_ids for
    /// new/changed files. New doc_ids are always > all existing ids so posting
    /// lists remain sorted after push.
    fn build_incremental_delta(
        base_doc_count: u32,
        old_overlay: &OverlayView,
        new_files: Vec<(PathBuf, Arc<[u8]>)>,
        newly_changed: &HashSet<PathBuf>,
        removed_paths: &HashSet<PathBuf>,
    ) -> Result<Self, IndexError> {
        // Clone old gram_index; remove stale entries for changed/deleted files.
        //
        // Cost: O(gram_index.len()), NOT O(changed files). For an overlay with
        // 1000 dirty files x ~120 grams = 120K entries, this is ~1 MB of
        // HashMap clone per commit_batch call regardless of how many files changed.
        // The delta path was optimised for the single-file edit case: the clone
        // is fast in practice (< 5 ms for typical overlay sizes), but grows
        // linearly with overlay size and eventually dominates the delta advantage.
        //
        // Mitigation (v2): use a persistent/CoW map (e.g., `im::HashMap`) so only
        // the changed entries are copied. See ARCHITECTURE.md "Overlay compaction".
        let mut gram_index = old_overlay.gram_index.clone();
        let overlay_docs =
            (old_overlay.docs.len() + new_files.len()).saturating_sub(newly_changed.len());

        for old_doc in &old_overlay.docs {
            if removed_paths.contains(&old_doc.path) || newly_changed.contains(&old_doc.path) {
                for &gram_hash in &old_doc.grams {
                    if let Some(list) = gram_index.get_mut(&gram_hash) {
                        list.retain(|&id| id != old_doc.doc_id);
                    }
                }
            }
        }
        // Drop posting lists that became empty after removal.
        gram_index.retain(|_, list| !list.is_empty());

        // Carry forward unchanged docs with their existing (stable) doc_ids.
        let mut docs: Vec<OverlayDoc> = old_overlay
            .docs
            .iter()
            .filter(|d| !removed_paths.contains(&d.path) && !newly_changed.contains(&d.path))
            .cloned()
            .collect();

        // New/changed files get fresh doc_ids starting from next_doc_id.
        // Since next_doc_id > all existing doc_ids, push keeps posting lists sorted.
        let mut next_id = old_overlay.next_doc_id;
        for (path, content) in new_files {
            let doc_id = Self::next_doc_id(&mut next_id, base_doc_count, overlay_docs)?;

            let grams = build_all(&content);
            for &gram_hash in &grams {
                gram_index.entry(gram_hash).or_default().push(doc_id);
            }
            docs.push(OverlayDoc {
                doc_id,
                path,
                content,
                grams,
            });
        }

        // Verify sorted-order invariant: new doc_ids > all existing, so push() keeps lists sorted.
        #[cfg(debug_assertions)]
        for (gram_hash, ids) in &gram_index {
            debug_assert!(
                ids.windows(2).all(|w| w[0] < w[1]),
                "posting list for gram {gram_hash:#x} is not strictly sorted after delta build: {ids:?}"
            );
        }

        let doc_id_map = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (d.doc_id, i))
            .collect();

        Ok(OverlayView {
            gram_index,
            docs,
            doc_id_map,
            next_doc_id: next_id,
            base_doc_count,
        })
    }

    /// Look up an overlay doc by its global doc_id. O(1) via HashMap.
    pub fn get_doc(&self, global_id: u32) -> Option<&OverlayDoc> {
        self.doc_id_map.get(&global_id).map(|&idx| &self.docs[idx])
    }

    /// Look up an overlay doc by path.
    pub fn get_doc_by_path(&self, path: &Path) -> Option<&OverlayDoc> {
        self.docs.iter().find(|d| d.path == path)
    }
}

// Re-export pending types so callers using `crate::index::overlay::*` continue to compile.
pub use crate::index::pending::{compute_delete_set, PendingEdits, TakeResult};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn incremental_delta_posting_lists_are_sorted() {
        let content_a: Arc<[u8]> = Arc::from(b"fn alpha_one() {}".as_slice());
        let content_b: Arc<[u8]> = Arc::from(b"fn beta_two() {}".as_slice());
        let overlay1 = OverlayView::build(
            0,
            vec![
                (PathBuf::from("a.rs"), Arc::clone(&content_a)),
                (PathBuf::from("b.rs"), Arc::clone(&content_b)),
            ],
        )
        .unwrap();

        let content_a2: Arc<[u8]> = Arc::from(b"fn alpha_changed() {}".as_slice());
        let newly_changed: HashSet<PathBuf> = [PathBuf::from("a.rs")].into();
        let removed: HashSet<PathBuf> = HashSet::new();

        let overlay2 = OverlayView::build_incremental(
            0,
            &overlay1,
            vec![(PathBuf::from("a.rs"), content_a2)],
            &newly_changed,
            &removed,
        )
        .unwrap();

        for (hash, ids) in &overlay2.gram_index {
            assert!(
                ids.windows(2).all(|w| w[0] < w[1]),
                "gram {hash:#x} posting list is not strictly sorted: {ids:?}"
            );
        }
    }

    #[test]
    fn build_incremental_no_underflow() {
        // B05: overlay_docs count uses saturating_sub so that degenerate inputs
        // (newly_changed.len() > old.len() + new_files.len()) produce 0, not
        // usize::MAX. Using base_doc_count=1 != old.base_doc_count=0 forces
        // the full rebuild path rather than the fast delta path.
        let content_a: Arc<[u8]> = Arc::from(b"fn alpha() {}".as_slice());
        let overlay1 = OverlayView::build(
            0,
            vec![(PathBuf::from("a.rs"), Arc::clone(&content_a))],
        )
        .unwrap();
        assert_eq!(overlay1.docs.len(), 1);

        // old has 1 doc, new_files is empty, but newly_changed has 2 paths.
        // (1 + 0).saturating_sub(2) = 0, not usize::MAX.
        let newly_changed: HashSet<PathBuf> =
            [PathBuf::from("a.rs"), PathBuf::from("ghost.rs")].into();
        let removed: HashSet<PathBuf> = HashSet::new();

        let result = OverlayView::build_incremental(
            1, // different from old.base_doc_count=0 → full rebuild, not delta
            &overlay1,
            vec![],
            &newly_changed,
            &removed,
        );
        assert!(result.is_ok(), "must not panic or error");
        assert_eq!(
            result.unwrap().docs.len(),
            0,
            "all old docs are in newly_changed with no replacements → empty overlay"
        );
    }
}
