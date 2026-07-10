use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::tokenizer::build_all;
use crate::IndexError;

use super::{OverlayDoc, OverlayView};

impl OverlayView {
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

        // Sorted-order invariant: new doc_ids > all existing, so push() keeps lists sorted.
        for ids in gram_index.values_mut() {
            if ids.windows(2).any(|w| w[0] >= w[1]) {
                ids.sort_unstable();
                ids.dedup();
            }
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
}
