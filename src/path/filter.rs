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
/// - `path_glob`: simple glob-style match on the full relative path.
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
    if path_glob.is_some() {
        let mut glob_bitmap = RoaringBitmap::new();
        for (file_id, path) in path_index.visible_paths() {
            if matches_path_filter(path, file_type, exclude_type, path_glob) {
                glob_bitmap.insert(file_id);
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
                    for (file_id, _) in path_index.visible_paths() {
                        all.insert(file_id);
                    }
                    all -= ext_bitmap;
                    all
                }
            });
        }
    }

    result.map(|file_ids| PathFilter { file_ids })
}

/// Check whether a path satisfies the same file type and path-glob semantics
/// used by `build_filter`.
pub(crate) fn matches_path_filter(
    path: &str,
    file_type: Option<&str>,
    exclude_type: Option<&str>,
    path_glob: Option<&str>,
) -> bool {
    if let Some(ext) = file_type {
        if !path
            .rsplit('.')
            .next()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        {
            return false;
        }
    }

    if let Some(ext) = exclude_type {
        if path
            .rsplit('.')
            .next()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        {
            return false;
        }
    }

    if let Some(glob) = path_glob {
        if !path_matches_glob(path, glob) {
            return false;
        }
    }

    true
}

/// Check if a path matches a simple glob pattern.
///
/// Supports:
/// - `*.ext`: match files by extension
/// - `**/*.ext`: match files by extension (recursive)
/// - `dir/`: match directory prefix
/// - `src/foo`: paths containing this exact segment sequence (has slash)
/// - Bare word `test`: match as a whole path component (filename or directory),
///   not as an arbitrary substring. Matches `test/`, `/test.rs`, `/test/`.
pub(crate) fn path_matches_glob(path: &str, glob: &str) -> bool {
    // "*.ext" pattern: match by extension
    if glob.starts_with("*.") && !glob.contains('/') {
        let ext = &glob[2..];
        return path
            .rsplit('.')
            .next()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    }

    // "**/*.ext" pattern: match any file with that extension
    if let Some(rest) = glob.strip_prefix("**/") {
        if rest.starts_with("*.") && !rest.contains('/') {
            let ext = &rest[2..];
            return path
                .rsplit('.')
                .next()
                .is_some_and(|e| e.eq_ignore_ascii_case(ext));
        }
        // Substring match on the rest
        return path.contains(rest);
    }

    // Directory prefix: "src/" matches "src/foo.rs"
    if glob.ends_with('/') {
        return path.starts_with(glob) || path.contains(&format!("/{glob}"));
    }

    // Contains a slash: treat as literal path substring (e.g. "src/test")
    if glob.contains('/') {
        return path.contains(glob);
    }

    // Bare word (no slash, no glob chars): match as a whole path component.
    // A component matches if its full name equals the word, or if the
    // filename stem (before the last dot) equals the word.
    path_has_component(path, glob)
}

/// Check if any path component matches `word` as a whole component.
/// Matches full component name or filename stem (before last dot).
fn path_has_component(path: &str, word: &str) -> bool {
    for component in path.split('/') {
        if component == word {
            return true;
        }
        // Match filename stem: "test.rs" matches "test"
        if let Some((stem, _)) = component.rsplit_once('.') {
            if stem == word {
                return true;
            }
        }
    }
    false
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

    #[test]
    fn matches_path_filter_combines_type_and_glob() {
        assert!(matches_path_filter(
            "src/main.rs",
            Some("rs"),
            None,
            Some("src/")
        ));
        assert!(!matches_path_filter(
            "src/main.py",
            Some("rs"),
            None,
            Some("src/")
        ));
        assert!(!matches_path_filter(
            "tests/main.rs",
            Some("rs"),
            None,
            Some("src/")
        ));
    }

    #[test]
    fn bare_word_glob_requires_component_boundary() {
        // "test" should match as a directory name or filename stem
        assert!(path_matches_glob("test/foo.rs", "test"));
        assert!(path_matches_glob("src/test.rs", "test"));
        assert!(path_matches_glob("src/test/util.rs", "test"));

        // Should NOT match as arbitrary substring
        assert!(
            !path_matches_glob("src/contest.rs", "test"),
            "bare word should not match as arbitrary substring"
        );
        assert!(
            !path_matches_glob("src/testing.rs", "test"),
            "bare word should match whole components only"
        );
    }

    #[test]
    fn path_with_slash_still_uses_substring() {
        // Patterns containing a slash use substring match (existing behavior)
        assert!(path_matches_glob("src/test/foo.rs", "src/test"));
        assert!(!path_matches_glob("lib/test/foo.rs", "src/test"));
    }
}
