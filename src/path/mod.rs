//! Path index: component-based Roaring bitmap sets for fast path/type filtering.
//!
//! `PathIndex` maps file extensions and path components to sets of file_ids.
//! A file_id is the index of the file's path in the sorted `paths` vector.

pub mod filter;

use std::collections::HashMap;

use roaring::RoaringBitmap;

/// Component-based path and type index.
///
/// Built once during `Index::build()` from the full sorted list of indexed paths.
/// Queried by `filter.rs` (Phase 6) to produce a `RoaringBitmap` of candidate file_ids.
pub struct PathIndex {
    /// All indexed paths, sorted lexicographically. `file_id = position`.
    pub paths: Vec<String>,
    /// File extension (e.g. "rs") -> set of file_ids with that extension.
    pub extension_to_files: HashMap<String, RoaringBitmap>,
    /// Path component (e.g. "api") -> set of file_ids containing that component.
    pub component_to_files: HashMap<String, RoaringBitmap>,
}

impl PathIndex {
    /// Build the path index from a sorted list of relative file paths.
    ///
    /// `sorted_paths` must be sorted in ascending order; `file_id` is the index
    /// into this slice. Duplicate paths are not allowed.
    pub fn build(sorted_paths: &[String]) -> Self {
        let mut extension_to_files: HashMap<String, RoaringBitmap> = HashMap::new();
        let mut component_to_files: HashMap<String, RoaringBitmap> = HashMap::new();

        for (file_id, path) in sorted_paths.iter().enumerate() {
            let file_id = file_id as u32;
            let p = std::path::Path::new(path);

            // Extension
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                extension_to_files
                    .entry(ext.to_ascii_lowercase())
                    .or_default()
                    .insert(file_id);
            }

            // All path components (directory names + filename stem)
            for component in p.components() {
                use std::path::Component;
                let s = match component {
                    Component::Normal(c) => c.to_str(),
                    _ => None,
                };
                if let Some(s) = s {
                    component_to_files
                        .entry(s.to_ascii_lowercase())
                        .or_default()
                        .insert(file_id);
                }
            }
        }

        PathIndex {
            paths: sorted_paths.to_vec(),
            extension_to_files,
            component_to_files,
        }
    }

    /// Return the file_id for an exact path, or `None` if not indexed.
    pub fn file_id(&self, path: &str) -> Option<u32> {
        self.paths.binary_search_by(|p| p.as_str().cmp(path)).ok().map(|i| i as u32)
    }

    /// Return the path for a given file_id, or `None` if out of range.
    pub fn path(&self, file_id: u32) -> Option<&str> {
        self.paths.get(file_id as usize).map(String::as_str)
    }

    /// All file_ids for files with the given extension (case-insensitive).
    pub fn files_with_extension(&self, ext: &str) -> Option<&RoaringBitmap> {
        self.extension_to_files.get(&ext.to_ascii_lowercase())
    }

    /// All file_ids for files whose path contains `component` (case-insensitive).
    pub fn files_with_component(&self, component: &str) -> Option<&RoaringBitmap> {
        self.component_to_files.get(&component.to_ascii_lowercase())
    }

    /// Build a global doc_id -> file_id mapping for O(1) path filter lookup.
    ///
    /// `resolve_path` maps each global doc_id to its relative path string.
    /// Returns a vec indexed by global doc_id; value is `u32::MAX` if unmapped.
    pub fn build_doc_to_file_id(
        &self,
        total_ids: usize,
        resolve_path: impl Fn(u32) -> Option<String>,
    ) -> Vec<u32> {
        let mut map = vec![u32::MAX; total_ids];
        for gid in 0..total_ids as u32 {
            if let Some(path) = resolve_path(gid) {
                if let Some(fid) = self.file_id(&path) {
                    map[gid as usize] = fid;
                }
            }
        }
        map
    }
}
