//! In-memory index for WASM targets (and, under the `ffi` feature, the C ABI).
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

/// Validate a caller-supplied document id and convert it to a path.
///
/// Same rules as `Index::repo_relative_path`: reject traversal (`..`),
/// absolute, and prefix components. This is sufficient even without
/// canonicalization: pure-overlay indexes have no filesystem, so there are no
/// symlinks to resolve, and `Component::ParentDir` catches all ".." segments
/// regardless of encoding. Empty ids are rejected too (they index nothing and
/// cannot be addressed).
///
/// Ids are also rejected if any `/`-separated segment is empty or `.`
/// (trailing/doubled slashes, `./`, or a bare `.`): `PathBuf`'s `Component`
/// iterator normalizes these away, so two distinct id strings like
/// `"chats/1"` and `"chats/1/"` would otherwise compare and hash as the same
/// `Path`, aliasing two different documents onto one path-index entry.
/// Checking the raw string's segments (rather than the normalized
/// `Component`s) catches this before it happens.
pub(crate) fn validate_doc_id(id: &str) -> Result<PathBuf, IndexError> {
    let path = PathBuf::from(id);
    let degenerate = id
        .split('/')
        .any(|segment| segment.is_empty() || segment == ".");
    if degenerate || crate::path_util::has_disallowed_component(&path) {
        return Err(IndexError::PathOutsideRepo(path));
    }
    Ok(path)
}

/// Build a pure-overlay snapshot from caller-supplied content.
///
/// Applies encoding normalization, the binary-skip heuristic, id validation,
/// and overlay/path-index construction with `base_doc_count = 0` (overlay
/// doc_ids start at 0; no base segments). Shared by the wasm-bindgen
/// `InMemoryIndex` and the FFI `MemIndex`.
pub(crate) fn build_overlay_snapshot(
    files: &HashMap<String, Arc<[u8]>>,
) -> Result<Arc<IndexSnapshot>, IndexError> {
    // Validate and filter files.
    let mut dirty_files: Vec<(PathBuf, Arc<[u8]>)> = Vec::with_capacity(files.len());
    for (rel_str, raw) in files {
        let path = validate_doc_id(rel_str)?;
        let content = crate::index::normalize_encoding(raw);
        if is_binary(&content) {
            continue;
        }
        // Avoid a second byte copy when normalization is a true no-op. A
        // `Cow::Borrowed` of a *shorter* slice than `raw` (e.g. BOM-stripped
        // content) is still a real change and must not be re-widened back to
        // the full `raw` bytes.
        let content: Arc<[u8]> = match content {
            std::borrow::Cow::Borrowed(b) if b.len() == raw.len() => Arc::clone(raw),
            std::borrow::Cow::Borrowed(b) => Arc::from(b),
            std::borrow::Cow::Owned(v) => Arc::from(v),
        };
        dirty_files.push((path, content));
    }

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
        base_doc_to_file_id: std::sync::OnceLock::new(),
    });

    Ok(Arc::new(new_snapshot(
        base,
        overlay,
        RoaringBitmap::new(),
        path_index,
        overlay_doc_to_file_id,
        0.10,
    )))
}

/// A fully in-memory index built from caller-provided file content.
///
/// Designed for the WASM target where no filesystem is available.
/// All documents live in the overlay so the resolver returns in-memory
/// content without any disk I/O. Immutable: build once, search many. For a
/// mutable variant see [`crate::index::mem_index::MemIndex`] (ffi feature).
pub struct InMemoryIndex {
    snapshot: Arc<IndexSnapshot>,
    config: Config,
}

impl InMemoryIndex {
    /// Build an in-memory index from a map of `repo_relative_path -> content`.
    pub fn build(files: HashMap<String, Vec<u8>>) -> Result<Self, IndexError> {
        let owned: HashMap<String, Arc<[u8]>> = files
            .into_iter()
            .map(|(k, v)| (k, Arc::from(v.into_boxed_slice())))
            .collect();
        let snapshot = build_overlay_snapshot(&owned)?;
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
    fn build_rejects_empty_path() {
        let mut files = HashMap::new();
        files.insert(String::new(), b"orphan".to_vec());
        expect_path_outside(InMemoryIndex::build(files));
    }

    #[test]
    fn build_accepts_clean_relative_paths() {
        let mut files = HashMap::new();
        files.insert("src/main.rs".into(), b"fn main() {}".to_vec());
        files.insert("lib/util.rs".into(), b"pub fn hello() {}".to_vec());
        assert!(InMemoryIndex::build(files).is_ok());
    }

    #[test]
    fn build_rejects_degenerate_separator_ids() {
        for bad in ["chats/1/", "chats//1", "chats/./1", "."] {
            let mut files = HashMap::new();
            files.insert(bad.to_string(), b"content".to_vec());
            expect_path_outside(InMemoryIndex::build(files));
        }
    }

    #[test]
    fn build_strips_bom_even_on_zero_copy_path() {
        let mut files = HashMap::new();
        // BOM (0xEF 0xBB 0xBF) immediately followed by content: this takes
        // normalize_encoding's Cow::Borrowed(rest) branch (a strict sub-slice
        // of the raw bytes), which must not be re-widened back to the full
        // raw bytes including the BOM.
        let mut content = vec![0xEF, 0xBB, 0xBF];
        content.extend_from_slice(b"needle here");
        files.insert("a.txt".into(), content);
        let idx = InMemoryIndex::build(files).expect("build");
        let matches = idx
            .search("needle", &SearchOptions::default())
            .expect("search");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].byte_offset, 0, "BOM bytes leaked into offsets");
        assert_eq!(matches[0].line_content, b"needle here");
    }
}
