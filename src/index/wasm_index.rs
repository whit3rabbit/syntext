//! In-memory index for WASM targets.
//!
//! Instead of writing segments to disk and mmap'ing them back, all files are
//! stored as `OverlayDoc` entries so the resolver can return content from memory
//! without any filesystem access.

use std::collections::HashMap;
use std::path::{Component, PathBuf};
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
        // Validate and filter files.
        let mut dirty_files: Vec<(PathBuf, Arc<[u8]>)> = Vec::with_capacity(files.len());
        for (rel_str, raw) in &files {
            let path = PathBuf::from(rel_str);
            // Reject traversal, absolute, and prefix paths (same rules as
            // Index::repo_relative_path). This is sufficient even without
            // canonicalization: WASM has no filesystem, so there are no symlinks
            // to resolve, and Component::ParentDir catches all ".." segments
            // regardless of encoding.
            if path.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            }) {
                return Err(IndexError::PathOutsideRepo(path));
            }
            let content = crate::index::normalize_encoding(raw, false);
            if is_binary(&content) {
                continue;
            }
            dirty_files.push((path, Arc::from(content.as_ref())));
        }

        // base_doc_count = 0: overlay doc_ids start at 0 (no base segments).
        // build() consumes dirty_files; extract paths from overlay.docs afterward.
        let overlay = OverlayView::build(0, dirty_files)?;

        let mut all_paths: Vec<PathBuf> = overlay.docs.iter().map(|d| d.path.clone()).collect();
        all_paths.sort_unstable();
        all_paths.dedup();
        let path_index = PathIndex::build(&all_paths);

        let mut overlay_doc_to_file_id = HashMap::new();
        for doc in &overlay.docs {
            if let Some(fid) = path_index.file_id(&doc.path) {
                overlay_doc_to_file_id.insert(doc.doc_id, fid);
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
            Arc::new(Vec::new()),
            overlay_doc_to_file_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_path_outside(result: Result<InMemoryIndex, IndexError>) {
        match result {
            Err(IndexError::PathOutsideRepo(_)) => {}
            Err(e) => panic!("expected PathOutsideRepo, got: {e}"),
            Ok(_) => panic!("expected PathOutsideRepo, got Ok"),
        }
    }

    #[test]
    fn build_rejects_parent_dir_traversal() {
        let mut files = HashMap::new();
        files.insert("../../etc/passwd".into(), b"root:x:0:0".to_vec());
        expect_path_outside(InMemoryIndex::build(files));
    }

    #[test]
    fn build_rejects_absolute_path() {
        let mut files = HashMap::new();
        files.insert("/etc/passwd".into(), b"root:x:0:0".to_vec());
        expect_path_outside(InMemoryIndex::build(files));
    }

    #[test]
    fn build_rejects_embedded_traversal() {
        let mut files = HashMap::new();
        files.insert("src/../../../etc/shadow".into(), b"secret".to_vec());
        expect_path_outside(InMemoryIndex::build(files));
    }

    #[test]
    fn build_accepts_clean_relative_paths() {
        let mut files = HashMap::new();
        files.insert("src/main.rs".into(), b"fn main() {}".to_vec());
        files.insert("lib/util.rs".into(), b"pub fn hello() {}".to_vec());
        assert!(InMemoryIndex::build(files).is_ok());
    }
}
