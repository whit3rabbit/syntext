//! Path/type glob filter using Roaring bitmaps from PathIndex.
//!
//! Produces a candidate file_id set that restricts which documents
//! enter the verification stage.

use std::path::Path;

use memchr::memmem;
use roaring::RoaringBitmap;

use crate::path_util::path_bytes;

use super::{ByteSplitExt, PathIndex};

/// A resolved path filter: a Roaring bitmap of matching file_ids.
pub struct PathFilter {
    /// Matching file_ids. Only documents in this set should be verified.
    pub file_ids: RoaringBitmap,
}

/// Build a `PathFilter` from search options against the given `PathIndex`.
///
/// - `file_types`: include only files with one of these extensions (e.g.
///   `["rs", "py"]`). Multiple types are UNIONed, so the Roaring extension
///   index narrows the candidate set even for `-t rs -t py`. Empty = no include
///   constraint.
/// - `exclude_types`: exclude files with any of these extensions. Empty = none.
/// - `path_glob`: simple glob-style match on the full relative path.
///
/// Returns `None` if no filter applies (all files are candidates).
pub fn build_filter(
    path_index: &PathIndex,
    file_types: &[&str],
    exclude_types: &[&str],
    path_glob: Option<&str>,
    glob_cache: Option<&std::sync::Mutex<std::collections::HashMap<String, RoaringBitmap>>>,
) -> Option<PathFilter> {
    let mut result: Option<RoaringBitmap> = None;

    if !file_types.is_empty() {
        // Union the extension bitmaps of every requested include type. A type
        // with no indexed files contributes nothing; if none of the types match
        // anything the union is empty and the filter matches no docs (same as
        // the old single-type `unwrap_or_default()` behavior).
        let mut inc = RoaringBitmap::new();
        for ext in file_types {
            if let Some(b) = path_index.files_with_extension(ext) {
                inc |= b;
            }
        }
        result = Some(match result {
            Some(r) => r & &inc,
            None => inc,
        });
    }

    if let Some(glob) = path_glob {
        let glob_bitmap = if let Some(cache) = glob_cache {
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = guard.get(glob) {
                cached.clone()
            } else {
                let mut glob_bitmap = RoaringBitmap::new();
                for (file_id, path) in path_index.visible_paths() {
                    if path_matches_glob(path, glob) {
                        glob_bitmap.insert(file_id);
                    }
                }
                guard.insert(glob.to_string(), glob_bitmap.clone());
                glob_bitmap
            }
        } else {
            let mut glob_bitmap = RoaringBitmap::new();
            for (file_id, path) in path_index.visible_paths() {
                if path_matches_glob(path, glob) {
                    glob_bitmap.insert(file_id);
                }
            }
            glob_bitmap
        };
        result = Some(match result {
            Some(r) => r & &glob_bitmap,
            None => glob_bitmap,
        });
    }

    if !exclude_types.is_empty() {
        // Subtract each excluded extension. If nothing has constrained the set
        // yet (exclude-only query), start from all visible docs.
        let mut base = result.unwrap_or_else(|| {
            let mut all = RoaringBitmap::new();
            for (file_id, _) in path_index.visible_paths() {
                all.insert(file_id);
            }
            all
        });
        for ext in exclude_types {
            if let Some(ext_bitmap) = path_index.files_with_extension(ext) {
                base -= ext_bitmap;
            }
        }
        result = Some(base);
    }

    result.map(|file_ids| PathFilter { file_ids })
}

/// Check whether a path satisfies the same file type and path-glob semantics
/// used by `build_filter`.
pub(crate) fn matches_path_filter(
    path: &Path,
    file_types: &[&str],
    exclude_types: &[&str],
    path_glob: Option<&str>,
) -> bool {
    let path_bytes = path_bytes(path);
    let path_bytes = path_bytes.as_ref();

    // Include: must match at least one requested type (empty = no constraint).
    if !file_types.is_empty()
        && !file_types
            .iter()
            .any(|ext| path_has_extension(path_bytes, ext.as_bytes()))
    {
        return false;
    }

    // Exclude: must not match any excluded type.
    if exclude_types
        .iter()
        .any(|ext| path_has_extension(path_bytes, ext.as_bytes()))
    {
        return false;
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
///
/// # Divergence from the CLI `-g` matcher (intentional)
///
/// This hand-rolled matcher is the INTERNAL path-glob used by `build_filter` /
/// `SearchOptions.path_filter`. It is NOT the same as the user-facing `-g`/
/// `--glob` matcher (`cli::scope::matches_optional_glob`, globset-backed with
/// `literal_separator`). They differ on a slashed pattern with no wildcard: a
/// pattern like `src/foo` substring-matches `src/foo/bar.rs` here, but the
/// globset `-g` matcher requires the whole path to equal `src/foo`. Keep them
/// separate; the divergence is locked by
/// `cli::scope_tests::glob_matchers_diverge_on_slash_prefix`.
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
        // Bare word (no '/' and no '*'): use component-boundary match, not
        // substring. "**/test" must NOT match "src/contest.rs".
        // Patterns with '/' (e.g., "**/src/test") keep substring semantics.
        if !rest.contains(&b'/') && !rest.contains(&b'*') {
            return path_has_component(path, rest);
        }
        return memmem::find(path, rest).is_some();
    }

    if glob.contains(&b'*') || glob.contains(&b'?') {
        if glob.contains(&b'/') {
            return path_glob_matches(path, glob);
        }
        return path
            .split(|&b| b == b'/')
            .any(|component| glob_matches_bytes(component, glob));
    }

    if glob.ends_with(b"/") {
        return path.starts_with(glob) || memmem::find(path, &[b"/", glob].concat()).is_some();
    }

    if glob.contains(&b'/') {
        return memmem::find(path, glob).is_some();
    }

    path_has_component(path, glob)
}

fn path_glob_matches(path: &[u8], glob: &[u8]) -> bool {
    if glob_matches_bytes(path, glob) {
        return true;
    }
    path.iter()
        .enumerate()
        .filter_map(|(idx, byte)| (*byte == b'/').then_some(idx + 1))
        .any(|start| glob_matches_bytes(&path[start..], glob))
}

fn glob_matches_bytes(text: &[u8], pattern: &[u8]) -> bool {
    let mut text_idx = 0usize;
    let mut pattern_idx = 0usize;
    let mut star_idx = None::<usize>;
    let mut star_text_idx = 0usize;

    while text_idx < text.len() {
        if pattern_idx < pattern.len()
            && (pattern[pattern_idx] == text[text_idx] || pattern[pattern_idx] == b'?')
        {
            text_idx += 1;
            pattern_idx += 1;
        } else if pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
            star_idx = Some(pattern_idx);
            pattern_idx += 1;
            star_text_idx = text_idx;
        } else if let Some(star) = star_idx {
            pattern_idx = star + 1;
            star_text_idx += 1;
            text_idx = star_text_idx;
        } else {
            return false;
        }
    }

    while pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
        pattern_idx += 1;
    }

    pattern_idx == pattern.len()
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

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
