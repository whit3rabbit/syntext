//! Path index: component-based Roaring bitmap sets for fast path/type filtering.
//!
//! `PathIndex` maps file extensions and path components to sets of file_ids.
//! A file_id is the index of the file's path in the sorted `paths` vector.

pub mod filter;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use roaring::RoaringBitmap;

use crate::path_util::path_bytes;

/// Component-based path and type index.
///
/// Built once during `Index::build()` from the full sorted list of indexed paths.
/// Queried by `filter.rs` (Phase 6) to produce a `RoaringBitmap` of candidate file_ids.
#[derive(Clone)]
pub struct PathIndex {
    /// All indexed paths, sorted lexicographically. `file_id = position`.
    pub paths: Vec<PathBuf>,
    /// Exact path -> file_id for O(1) lookups.
    path_to_file_id: HashMap<PathBuf, u32>,
    /// Stable file_id -> path. Deleted entries are `None`.
    file_id_to_path: Vec<Option<PathBuf>>,
    /// File extension (e.g. "rs") -> set of file_ids with that extension.
    pub extension_to_files: HashMap<Vec<u8>, RoaringBitmap>,
    /// Path component (e.g. "api") -> set of file_ids containing that component.
    pub component_to_files: HashMap<Vec<u8>, RoaringBitmap>,
    /// Next fresh stable file_id.
    next_file_id: u32,
}

impl PathIndex {
    /// Build the path index from a sorted list of relative file paths.
    ///
    /// `sorted_paths` must be sorted in ascending order; `file_id` is the index
    /// into this slice. Duplicate paths are not allowed.
    pub fn build(sorted_paths: &[PathBuf]) -> Self {
        let mut path_to_file_id: HashMap<PathBuf, u32> = HashMap::with_capacity(sorted_paths.len());
        let mut file_id_to_path: Vec<Option<PathBuf>> = Vec::with_capacity(sorted_paths.len());
        let mut extension_to_files: HashMap<Vec<u8>, RoaringBitmap> = HashMap::new();
        let mut component_to_files: HashMap<Vec<u8>, RoaringBitmap> = HashMap::new();

        for (file_id, path) in sorted_paths.iter().enumerate() {
            let file_id = file_id as u32;
            path_to_file_id.insert(path.clone(), file_id);
            file_id_to_path.push(Some(path.clone()));
            insert_path_metadata(
                &mut extension_to_files,
                &mut component_to_files,
                file_id,
                path,
            );
        }

        PathIndex {
            paths: sorted_paths.to_vec(),
            path_to_file_id,
            file_id_to_path,
            extension_to_files,
            component_to_files,
            next_file_id: sorted_paths.len() as u32,
        }
    }

    /// Incrementally update the path index while preserving stable file IDs
    /// for unchanged paths.
    pub fn build_incremental(
        old: &PathIndex,
        removed_paths: &HashSet<PathBuf>,
        added_paths: &HashSet<PathBuf>,
    ) -> Self {
        let mut paths = old.paths.clone();
        paths.retain(|path| !removed_paths.contains(path));

        let mut path_to_file_id = old.path_to_file_id.clone();
        let mut file_id_to_path = old.file_id_to_path.clone();
        let mut extension_to_files = old.extension_to_files.clone();
        let mut component_to_files = old.component_to_files.clone();
        for path in removed_paths {
            if let Some(file_id) = path_to_file_id.remove(path) {
                file_id_to_path[file_id as usize] = None;
                remove_path_metadata(
                    &mut extension_to_files,
                    &mut component_to_files,
                    file_id,
                    path,
                );
            }
        }

        let mut next_file_id = old.next_file_id;
        for path in added_paths {
            if path_to_file_id.contains_key(path) {
                continue;
            }
            let file_id = next_file_id;
            next_file_id += 1;
            path_to_file_id.insert(path.clone(), file_id);
            file_id_to_path.push(Some(path.clone()));
            paths.push(path.clone());
            insert_path_metadata(
                &mut extension_to_files,
                &mut component_to_files,
                file_id,
                path,
            );
        }

        paths.sort_unstable();
        paths.dedup();

        PathIndex {
            paths,
            path_to_file_id,
            file_id_to_path,
            extension_to_files,
            component_to_files,
            next_file_id,
        }
    }

    /// Return the file_id for an exact path, or `None` if not indexed.
    pub fn file_id(&self, path: &Path) -> Option<u32> {
        self.path_to_file_id.get(path).copied()
    }

    /// Return the path for a given file_id, or `None` if out of range.
    pub fn path(&self, file_id: u32) -> Option<&Path> {
        self.file_id_to_path
            .get(file_id as usize)
            .and_then(|path| path.as_deref())
    }

    /// Iterate the visible `(file_id, path)` pairs.
    pub fn visible_paths(&self) -> impl Iterator<Item = (u32, &Path)> {
        self.file_id_to_path
            .iter()
            .enumerate()
            .filter_map(|(file_id, path)| path.as_deref().map(|path| (file_id as u32, path)))
    }

    /// All file_ids for files with the given extension (case-insensitive).
    pub fn files_with_extension(&self, ext: &str) -> Option<&RoaringBitmap> {
        self.extension_to_files.get(&ascii_lower(ext.as_bytes()))
    }

    /// All file_ids for files whose path contains `component` (case-insensitive).
    pub fn files_with_component(&self, component: &str) -> Option<&RoaringBitmap> {
        self.component_to_files.get(&ascii_lower(component.as_bytes()))
    }

    /// Build a global doc_id -> file_id mapping for O(1) path filter lookup.
    ///
    /// `resolve_path` maps each global doc_id to its relative path.
    /// Returns a vec indexed by global doc_id; value is `u32::MAX` if unmapped.
    pub fn build_doc_to_file_id(
        &self,
        total_ids: usize,
        resolve_path: impl Fn(u32) -> Option<PathBuf>,
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

fn insert_path_metadata(
    extension_to_files: &mut HashMap<Vec<u8>, RoaringBitmap>,
    component_to_files: &mut HashMap<Vec<u8>, RoaringBitmap>,
    file_id: u32,
    path: &Path,
) {
    if let Some(ext) = path_extension(path) {
        extension_to_files.entry(ext).or_default().insert(file_id);
    }

    for component in path_components(path) {
        component_to_files
            .entry(component)
            .or_default()
            .insert(file_id);
    }
}

fn remove_path_metadata(
    extension_to_files: &mut HashMap<Vec<u8>, RoaringBitmap>,
    component_to_files: &mut HashMap<Vec<u8>, RoaringBitmap>,
    file_id: u32,
    path: &Path,
) {
    if let Some(ext) = path_extension(path) {
        if let Some(bitmap) = extension_to_files.get_mut(&ext) {
            bitmap.remove(file_id);
            if bitmap.is_empty() {
                extension_to_files.remove(&ext);
            }
        }
    }

    for component in path_components(path) {
        if let Some(bitmap) = component_to_files.get_mut(&component) {
            bitmap.remove(file_id);
            if bitmap.is_empty() {
                component_to_files.remove(&component);
            }
        }
    }
}

fn path_extension(path: &Path) -> Option<Vec<u8>> {
    let bytes = path_bytes(path);
    let name = bytes.rsplit(|&b| b == b'/').next()?;
    let (_, ext) = ByteSplitExt::rsplit_once(name, |&b| b == b'.')?;
    if ext.is_empty() {
        None
    } else {
        Some(ascii_lower(ext))
    }
}

fn path_components(path: &Path) -> Vec<Vec<u8>> {
    let bytes = path_bytes(path);
    bytes
        .split(|&b| b == b'/')
        .filter(|component| !component.is_empty())
        .map(ascii_lower)
        .collect()
}

fn ascii_lower(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(u8::to_ascii_lowercase).collect()
}

trait ByteSplitExt {
    fn rsplit_once<P>(&self, pred: P) -> Option<(&[u8], &[u8])>
    where
        P: FnMut(&u8) -> bool;
}

impl ByteSplitExt for [u8] {
    fn rsplit_once<P>(&self, pred: P) -> Option<(&[u8], &[u8])>
    where
        P: FnMut(&u8) -> bool,
    {
        let idx = self.iter().rposition(pred)?;
        Some((&self[..idx], &self[idx + 1..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_preserves_stable_ids_for_unchanged_paths() {
        let initial = PathIndex::build(&[PathBuf::from("src/lib.rs"), PathBuf::from("src/main.rs")]);
        let main_id = initial.file_id(Path::new("src/main.rs")).unwrap();

        let updated = PathIndex::build_incremental(
            &initial,
            &HashSet::from([PathBuf::from("src/lib.rs")]),
            &HashSet::from([PathBuf::from("src/new.rs")]),
        );

        assert_eq!(updated.file_id(Path::new("src/main.rs")), Some(main_id));
        assert!(updated.file_id(Path::new("src/new.rs")).unwrap() > main_id);
        assert_eq!(updated.path(main_id), Some(Path::new("src/main.rs")));
    }

    #[test]
    fn incremental_updates_extension_and_component_bitmaps() {
        let initial =
            PathIndex::build(&[PathBuf::from("src/lib.rs"), PathBuf::from("docs/readme.md")]);
        let lib_id = initial.file_id(Path::new("src/lib.rs")).unwrap();

        let updated = PathIndex::build_incremental(
            &initial,
            &HashSet::from([PathBuf::from("src/lib.rs")]),
            &HashSet::from([PathBuf::from("src/new.py")]),
        );

        assert!(!updated
            .files_with_extension("rs")
            .is_some_and(|bm| bm.contains(lib_id)));
        assert!(updated
            .files_with_extension("py")
            .is_some_and(|bm| bm.contains(updated.file_id(Path::new("src/new.py")).unwrap())));
        assert!(updated
            .files_with_component("new.py")
            .is_some_and(|bm| bm.contains(updated.file_id(Path::new("src/new.py")).unwrap())));
    }
}
