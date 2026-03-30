//! In-memory index for WASM targets.
//!
//! Instead of writing segments to disk and mmap'ing them back, all files are
//! stored as `OverlayDoc` entries so the resolver can return content from memory
//! without any filesystem access.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use roaring::RoaringBitmap;

use crate::index::overlay::OverlayView;
use crate::index::snapshot::{new_snapshot, BaseSegments, IndexSnapshot};
use crate::index::walk::is_binary;
use crate::path::PathIndex;
use crate::{Config, IndexError, SearchMatch, SearchOptions};

/// A fully in-memory index built from caller-provided file content.
///
/// Designed for the WASM target where no filesystem is available.
/// All documents live in the overlay so the resolver returns in-memory
/// content without any disk I/O.
pub struct InMemoryIndex {
    snapshot: Arc<IndexSnapshot>,
    config: Config,
}

impl InMemoryIndex {
    /// Build an in-memory index from a map of `repo_relative_path -> content`.
    pub fn build(files: HashMap<String, Vec<u8>>) -> Result<Self, IndexError> {
        // Normalize and filter binary files.
        let dirty_files: Vec<(PathBuf, Arc<[u8]>)> = files
            .iter()
            .filter_map(|(rel_str, raw)| {
                let content = crate::index::normalize_encoding(raw, false);
                if is_binary(&content) {
                    return None;
                }
                let path = PathBuf::from(rel_str);
                Some((path, Arc::from(content.as_ref())))
            })
            .collect();

        // base_doc_count = 0: overlay doc_ids start at 0 (no base segments).
        // build() consumes dirty_files; extract paths from overlay.docs afterward.
        let overlay = OverlayView::build(0, dirty_files)?;

        let mut all_paths: Vec<PathBuf> = overlay.docs.iter().map(|d| d.path.clone()).collect();
        all_paths.sort_unstable();
        all_paths.dedup();
        let path_index = PathIndex::build(&all_paths);

        let max_id = overlay.docs.iter().map(|d| d.doc_id).max().unwrap_or(0);
        let mut doc_to_file_id = vec![u32::MAX; (max_id + 1) as usize];
        for doc in &overlay.docs {
            if let Some(fid) = path_index.file_id(&doc.path) {
                doc_to_file_id[doc.doc_id as usize] = fid;
            }
        }

        let base = Arc::new(BaseSegments {
            segments: vec![],
            base_ids: vec![],
            base_doc_paths: vec![],
            path_doc_ids: HashMap::new(),
        });

        let snapshot = Arc::new(new_snapshot(
            base,
            overlay,
            RoaringBitmap::new(),
            path_index,
            doc_to_file_id,
            0.10,
        ));

        Ok(InMemoryIndex {
            snapshot,
            config: Config::default(),
        })
    }

    /// Search for `pattern` across all indexed files.
    pub fn search(
        &self,
        pattern: &str,
        opts: &SearchOptions,
    ) -> Result<Vec<SearchMatch>, IndexError> {
        // canonical_root is unused for pure-overlay snapshots (resolver returns
        // overlay content directly without touching the filesystem).
        let canonical_root = std::path::Path::new(".");
        crate::search::search(
            Arc::clone(&self.snapshot),
            &self.config,
            canonical_root,
            pattern,
            opts,
        )
    }
}
