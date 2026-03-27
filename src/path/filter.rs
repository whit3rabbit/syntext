//! Path/type glob filter using Roaring bitmaps from PathIndex.
//!
//! Produces a candidate file_id set that restricts which documents
//! enter the verification stage.

use roaring::RoaringBitmap;

use super::PathIndex;

/// A resolved path filter: a Roaring bitmap of matching file_ids.
pub struct PathFilter {
    /// Matching file_ids. Only documents in this set should be verified.
    pub file_ids: RoaringBitmap,
}

/// Build a `PathFilter` from search options against the given `PathIndex`.
///
/// - `file_type`: include only files with this extension (e.g. "rs").
/// - `exclude_type`: exclude files with this extension (e.g. "js").
/// - `path_glob`: substring match on the full relative path.
///
/// Returns `None` if no filter applies (all files are candidates).
pub fn build_filter(
    path_index: &PathIndex,
    file_type: Option<&str>,
    exclude_type: Option<&str>,
    path_glob: Option<&str>,
) -> Option<PathFilter> {
    let mut result: Option<RoaringBitmap> = None;

    // File type inclusion: AND with extension bitmap.
    if let Some(ext) = file_type {
        let ext_bitmap = path_index
            .files_with_extension(ext)
            .cloned()
            .unwrap_or_default();
        result = Some(match result {
            Some(r) => r & &ext_bitmap,
            None => ext_bitmap,
        });
    }

    // Path glob: substring match on full path, build bitmap.
    if let Some(glob) = path_glob {
        let mut glob_bitmap = RoaringBitmap::new();
        for (i, path) in path_index.paths.iter().enumerate() {
            if path_matches_glob(path, glob) {
                glob_bitmap.insert(i as u32);
            }
        }
        result = Some(match result {
            Some(r) => r & &glob_bitmap,
            None => glob_bitmap,
        });
    }

    // File type exclusion: AND-NOT with extension bitmap.
    if let Some(ext) = exclude_type {
        if let Some(ext_bitmap) = path_index.files_with_extension(ext) {
            result = Some(match result {
                Some(mut r) => {
                    r -= ext_bitmap;
                    r
                }
                None => {
                    // Start with all files, then exclude.
                    let mut all = RoaringBitmap::new();
                    for i in 0..path_index.paths.len() as u32 {
                        all.insert(i);
                    }
                    all -= ext_bitmap;
                    all
                }
            });
        }
    }

    result.map(|file_ids| PathFilter { file_ids })
}

/// Check if a path matches a simple glob pattern.
///
/// Supports:
/// - Bare substring: "src/" matches any path containing "src/"
/// - Leading "**/" is treated as substring match
/// - Trailing "/*.ext" matches files in a directory with that extension
/// - Simple extension glob "*.rs" matches by extension
fn path_matches_glob(path: &str, glob: &str) -> bool {
    // "*.ext" pattern: match by extension
    if glob.starts_with("*.") && !glob.contains('/') {
        let ext = &glob[2..];
        return path.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case(ext));
    }

    // "**/*.ext" pattern: match any file with that extension
    if let Some(rest) = glob.strip_prefix("**/") {
        if rest.starts_with("*.") && !rest.contains('/') {
            let ext = &rest[2..];
            return path.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case(ext));
        }
        // Substring match on the rest
        return path.contains(rest);
    }

    // Directory prefix: "src/" matches "src/foo.rs"
    if glob.ends_with('/') {
        return path.starts_with(glob) || path.contains(&format!("/{glob}"));
    }

    // Default: substring match
    path.contains(glob)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_index() -> PathIndex {
        let paths = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "src/util.py".to_string(),
            "tests/test_main.rs".to_string(),
            "docs/readme.md".to_string(),
            "scripts/build.js".to_string(),
        ];
        PathIndex::build(&paths)
    }

    #[test]
    fn filter_by_extension() {
        let idx = make_index();
        let filter = build_filter(&idx, Some("rs"), None, None).unwrap();
        assert_eq!(filter.file_ids.len(), 3); // main.rs, lib.rs, test_main.rs
    }

    #[test]
    fn filter_by_path_glob() {
        let idx = make_index();
        let filter = build_filter(&idx, None, None, Some("src/")).unwrap();
        // src/main.rs, src/lib.rs, src/util.py
        assert_eq!(filter.file_ids.len(), 3);
    }

    #[test]
    fn filter_combined_type_and_path() {
        let idx = make_index();
        let filter = build_filter(&idx, Some("rs"), None, Some("src/")).unwrap();
        // Only src/*.rs: main.rs, lib.rs
        assert_eq!(filter.file_ids.len(), 2);
    }

    #[test]
    fn filter_exclude_type() {
        let idx = make_index();
        let filter = build_filter(&idx, None, Some("js"), None).unwrap();
        // Everything except build.js
        assert_eq!(filter.file_ids.len(), 5);
    }

    #[test]
    fn no_filter_returns_none() {
        let idx = make_index();
        let filter = build_filter(&idx, None, None, None);
        assert!(filter.is_none());
    }

    #[test]
    fn glob_star_extension() {
        assert!(path_matches_glob("src/main.rs", "*.rs"));
        assert!(!path_matches_glob("src/main.py", "*.rs"));
    }

    #[test]
    fn glob_double_star_extension() {
        assert!(path_matches_glob("deep/nested/file.rs", "**/*.rs"));
        assert!(!path_matches_glob("deep/nested/file.py", "**/*.rs"));
    }
}
