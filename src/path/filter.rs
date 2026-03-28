//! Path/type glob filter using Roaring bitmaps from PathIndex.
//!
//! Produces a candidate file_id set that restricts which documents
//! enter the verification stage.

use std::path::Path;

use memchr::memmem;
use roaring::RoaringBitmap;

use crate::path_util::path_bytes;

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

    if let Some(ext) = exclude_type {
        if let Some(ext_bitmap) = path_index.files_with_extension(ext) {
            result = Some(match result {
                Some(mut r) => {
                    r -= ext_bitmap;
                    r
                }
                None => {
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
    path: &Path,
    file_type: Option<&str>,
    exclude_type: Option<&str>,
    path_glob: Option<&str>,
) -> bool {
    let path_bytes = path_bytes(path);
    let path_bytes = path_bytes.as_ref();

    if let Some(ext) = file_type {
        if !path_has_extension(path_bytes, ext.as_bytes()) {
            return false;
        }
    }

    if let Some(ext) = exclude_type {
        if path_has_extension(path_bytes, ext.as_bytes()) {
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
pub(crate) fn path_matches_glob(path: &Path, glob: &str) -> bool {
    let path_bytes = path_bytes(path);
    let path = path_bytes.as_ref();
    let glob = glob.as_bytes();

    if glob.starts_with(b"*.") && !glob.contains(&b'/') {
        return path_has_extension(path, &glob[2..]);
    }

    if let Some(rest) = glob.strip_prefix(b"**/") {
        if rest.starts_with(b"*.") && !rest.contains(&b'/') {
            return path_has_extension(path, &rest[2..]);
        }
        return memmem::find(path, rest).is_some();
    }

    if glob.ends_with(b"/") {
        return path.starts_with(glob)
            || memmem::find(path, &[b"/", glob].concat()).is_some();
    }

    if glob.contains(&b'/') {
        return memmem::find(path, glob).is_some();
    }

    path_has_component(path, glob)
}

fn path_has_extension(path: &[u8], ext: &[u8]) -> bool {
    let Some(name) = path.rsplit(|&b| b == b'/').next() else {
        return false;
    };
    let Some((_, actual_ext)) = ByteSplitExt::rsplit_once(name, |&b| b == b'.') else {
        return false;
    };
    ascii_eq_ignore_case(actual_ext, ext)
}

fn path_has_component(path: &[u8], word: &[u8]) -> bool {
    for component in path.split(|&b| b == b'/') {
        if component == word {
            return true;
        }
        if let Some((stem, _)) = ByteSplitExt::rsplit_once(component, |&b| b == b'.') {
            if stem == word {
                return true;
            }
        }
    }
    false
}

fn ascii_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(l, r)| l.eq_ignore_ascii_case(r))
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

    fn make_index() -> PathIndex {
        let paths = vec![
            std::path::PathBuf::from("src/main.rs"),
            std::path::PathBuf::from("src/lib.rs"),
            std::path::PathBuf::from("src/util.py"),
            std::path::PathBuf::from("tests/test_main.rs"),
            std::path::PathBuf::from("docs/readme.md"),
            std::path::PathBuf::from("scripts/build.js"),
        ];
        PathIndex::build(&paths)
    }

    #[test]
    fn filter_by_extension() {
        let idx = make_index();
        let filter = build_filter(&idx, Some("rs"), None, None).unwrap();
        assert_eq!(filter.file_ids.len(), 3);
    }

    #[test]
    fn filter_by_path_glob() {
        let idx = make_index();
        let filter = build_filter(&idx, None, None, Some("src/")).unwrap();
        assert_eq!(filter.file_ids.len(), 3);
    }

    #[test]
    fn filter_combined_type_and_path() {
        let idx = make_index();
        let filter = build_filter(&idx, Some("rs"), None, Some("src/")).unwrap();
        assert_eq!(filter.file_ids.len(), 2);
    }

    #[test]
    fn filter_exclude_type() {
        let idx = make_index();
        let filter = build_filter(&idx, None, Some("js"), None).unwrap();
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
        assert!(path_matches_glob(Path::new("src/main.rs"), "*.rs"));
        assert!(!path_matches_glob(Path::new("src/main.py"), "*.rs"));
    }

    #[test]
    fn glob_double_star_extension() {
        assert!(path_matches_glob(Path::new("deep/nested/file.rs"), "**/*.rs"));
        assert!(!path_matches_glob(Path::new("deep/nested/file.py"), "**/*.rs"));
    }

    #[test]
    fn matches_path_filter_combines_type_and_glob() {
        assert!(matches_path_filter(
            Path::new("src/main.rs"),
            Some("rs"),
            None,
            Some("src/")
        ));
        assert!(!matches_path_filter(
            Path::new("src/main.py"),
            Some("rs"),
            None,
            Some("src/")
        ));
        assert!(!matches_path_filter(
            Path::new("tests/main.rs"),
            Some("rs"),
            None,
            Some("src/")
        ));
    }

    #[test]
    fn bare_word_glob_requires_component_boundary() {
        assert!(path_matches_glob(Path::new("test/foo.rs"), "test"));
        assert!(path_matches_glob(Path::new("src/test.rs"), "test"));
        assert!(path_matches_glob(Path::new("src/test/util.rs"), "test"));
        assert!(!path_matches_glob(Path::new("src/contest.rs"), "test"));
        assert!(!path_matches_glob(Path::new("src/testing.rs"), "test"));
    }

    #[test]
    fn path_with_slash_still_uses_substring() {
        assert!(path_matches_glob(Path::new("src/test/foo.rs"), "src/test"));
        assert!(!path_matches_glob(Path::new("lib/test/foo.rs"), "src/test"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_participate_in_extension_and_glob_filters() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = std::path::PathBuf::from(OsString::from_vec(b"src/odd\xff.rs".to_vec()));
        assert!(matches_path_filter(&path, Some("rs"), None, Some("src/")));
        assert!(path_matches_glob(&path, "*.rs"));
        assert!(path_matches_glob(&path, "src/"));
    }
}
