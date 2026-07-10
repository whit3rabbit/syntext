//! Path-scope filtering helpers: CLI path resolution, glob matching, explicit
//! path specs, file enumeration (--files mode), and result deduplication.

use std::path::{Component, Path, PathBuf};

use crate::index::Index;
use crate::path::filter::matches_path_filter;
use crate::{Config, SearchOptions};

use super::search::SearchArgs;

/// Count directory components in a relative path (0 = file at root).
pub(super) fn path_depth(path: &Path) -> usize {
    path.components().count().saturating_sub(1)
}

pub(super) fn truncate_matches_per_file(
    matches: Vec<crate::SearchMatch>,
    limit: usize,
) -> Vec<crate::SearchMatch> {
    let mut kept = Vec::with_capacity(matches.len().min(limit));
    let mut current_path: Option<PathBuf> = None;
    let mut kept_in_file = 0usize;

    for m in matches {
        if current_path.as_ref() != Some(&m.path) {
            current_path = Some(m.path.clone());
            kept_in_file = 0;
        }
        if kept_in_file < limit {
            kept.push(m);
            kept_in_file += 1;
        }
    }

    kept
}

#[derive(Clone)]
pub(super) struct ExplicitPathSpec {
    pub(super) rel_path: PathBuf,
    is_dir: bool,
}

impl ExplicitPathSpec {
    pub(super) fn path_filter(&self) -> String {
        let rel = self.rel_path.to_string_lossy();
        if self.is_dir {
            format!("{rel}/")
        } else {
            rel.into_owned()
        }
    }
}

pub(super) fn explicit_path_specs(repo_root: &Path, paths: &[PathBuf]) -> Vec<ExplicitPathSpec> {
    // Relative CLI paths resolve against CWD when CWD is inside the repo
    // (ripgrep semantics: `st pat src/` from a subdir scopes there, and `.` no
    // longer normalizes to an empty whole-repo search). When CWD is outside the
    // repo (explicit --repo-root), paths stay repo-root-relative. See
    // `resolve_relative_base`.
    let cwd = std::env::current_dir().unwrap_or_else(|_| repo_root.to_path_buf());
    paths
        .iter()
        .map(|path| ExplicitPathSpec {
            rel_path: relativize_cli_path(repo_root, &cwd, path),
            is_dir: path_is_directory(repo_root, &cwd, path),
        })
        // Drop specs whose rel_path is empty (e.g. "." or the repo root
        // itself).  An empty rel_path means "search everything", which is
        // the default when no paths are given.  Keeping it would pass "/"
        // as the index path filter, matching nothing.
        .filter(|spec| !spec.rel_path.as_os_str().is_empty())
        .collect()
}

pub(super) fn matches_any_explicit_path(path: &Path, specs: &[ExplicitPathSpec]) -> bool {
    specs.is_empty() || specs.iter().any(|spec| explicit_path_matches(path, spec))
}

fn explicit_path_matches(path: &Path, spec: &ExplicitPathSpec) -> bool {
    if spec.rel_path.as_os_str().is_empty() {
        return true;
    }
    if spec.is_dir {
        path.starts_with(&spec.rel_path)
    } else {
        path == spec.rel_path
    }
}

pub(super) fn shows_filename_by_default(config: &Config, paths: &[PathBuf]) -> bool {
    match explicit_path_specs(config.repo_root.as_path(), paths).as_slice() {
        [] => true,
        [spec] => spec.is_dir,
        _ => true,
    }
}

fn path_is_directory(repo_root: &Path, cwd: &Path, path: &Path) -> bool {
    cli_path_on_disk(repo_root, cwd, path)
        .metadata()
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

fn relativize_cli_path(repo_root: &Path, cwd: &Path, path: &Path) -> PathBuf {
    // Absolute paths: strip the repo root to get the repo-relative path.
    // Relative paths: see `resolve_relative_base` for the CWD-vs-repo-root rule.
    let base = if path.is_absolute() {
        path.to_path_buf()
    } else {
        resolve_relative_base(repo_root, cwd, path)
    };
    let rel = match base.strip_prefix(repo_root) {
        Ok(rel) => rel,
        Err(_) => base.as_path(),
    };
    crate::path_util::normalize_to_forward_slashes(normalize_relative_path(rel))
}

fn cli_path_on_disk(repo_root: &Path, cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        resolve_relative_base(repo_root, cwd, path)
    }
}

/// Pick the absolute base a relative CLI path resolves against.
///
/// ripgrep resolves relative paths against CWD, so when CWD is inside the repo
/// we do too: an agent standing in `<repo>/crates/foo` scopes `st pat src/` to
/// `crates/foo/src`, not `<root>/src`, and `st pat .` scopes to the subdir
/// instead of normalizing to an empty path that searches the whole repo.
///
/// When CWD is outside the repo (an explicit `--repo-root` pointing at a repo
/// the caller is not standing in), fall back to repo-root-relative resolution
/// so the path still reaches the index. This preserves the long-standing
/// `--repo-root <repo> <relpath>` contract.
fn resolve_relative_base(repo_root: &Path, cwd: &Path, path: &Path) -> PathBuf {
    let via_cwd = cwd.join(path);
    if via_cwd.starts_with(repo_root) {
        via_cwd
    } else {
        repo_root.join(path)
    }
}

fn normalize_relative_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => normalized.push(component.as_os_str()),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

pub(super) fn search_options(args: &SearchArgs, path_filter: Option<String>) -> SearchOptions {
    SearchOptions {
        case_insensitive: args.ignore_case,
        file_type: single_filter(&args.file_types),
        exclude_type: single_filter(&args.type_nots),
        max_results: None,
        path_filter,
        verify_pattern: None,
        #[cfg(any(test, feature = "oracle"))]
        force_full_scan: false,
    }
}

fn single_filter(filters: &[String]) -> Option<String> {
    if filters.len() == 1 {
        Some(filters[0].clone())
    } else {
        None
    }
}

pub(super) fn matches_optional_glob(
    path: &Path,
    file_types: &[String],
    exclude_types: &[String],
    path_globs: &[String],
) -> bool {
    if !file_types.is_empty()
        && !file_types
            .iter()
            .any(|file_type| matches_path_filter(path, Some(file_type.as_str()), None, None))
    {
        return false;
    }

    if exclude_types
        .iter()
        .any(|exclude_type| matches_path_filter(path, Some(exclude_type.as_str()), None, None))
    {
        return false;
    }

    if path_globs.is_empty() {
        return true;
    }

    // Use globset for correct glob semantics matching rg's -g behaviour:
    //
    //   • Patterns WITHOUT '/' are matched against the **basename** only,
    //     so `*.rs` matches `src/lib.rs` (not just files at the repo root).
    //     We achieve this by building without literal_separator and then
    //     testing against Path::file_name().
    //
    //   • Patterns WITH '/' are matched against the **full relative path**
    //     with literal_separator(true), so `src/foo` does NOT substring-match
    //     `mysrc/foo` (each slash is a component boundary).
    //
    // Both paths support [...] character classes and {a,b} alternation.
    use globset::{GlobBuilder, GlobSetBuilder};

    let basename = path.file_name().map(Path::new);

    let mut has_positive = false;
    let mut matched_positive = false;
    let mut excluded = false;

    for glob_str in path_globs {
        let (is_exclude, pattern) = if let Some(excl) = glob_str.strip_prefix('!') {
            (true, excl)
        } else {
            (false, glob_str.as_str())
        };

        if pattern.is_empty() {
            continue;
        }

        let has_slash = pattern.contains('/');

        // Build the glob. Patterns with '/' use literal_separator(true) so
        // slashes are real boundaries; basename patterns build without it.
        let glob_result = if has_slash {
            GlobBuilder::new(pattern).literal_separator(true).build()
        } else {
            GlobBuilder::new(pattern).build()
        };

        let Ok(glob) = glob_result else {
            // Malformed glob: for exclusions, conservatively keep the file;
            // for inclusions, treat as no-match (other patterns may still match).
            if !is_exclude {
                has_positive = true;
            }
            continue;
        };

        // Build a single-pattern set for matching.
        let mut builder = GlobSetBuilder::new();
        builder.add(glob);
        let Ok(set) = builder.build() else {
            if !is_exclude {
                has_positive = true;
            }
            continue;
        };

        // Determine the match target.
        let matches = if has_slash {
            set.is_match(path)
        } else {
            // Match against basename for patterns without '/'.
            basename.is_some_and(|b| set.is_match(b))
        };

        if is_exclude {
            if matches {
                excluded = true;
            }
        } else {
            has_positive = true;
            if matches {
                matched_positive = true;
            }
        }
    }

    if excluded {
        return false;
    }

    !has_positive || matched_positive
}

pub(super) fn collect_scoped_paths(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
) -> Vec<PathBuf> {
    let snapshot = index.snapshot();
    let explicit_specs = explicit_path_specs(config.repo_root.as_path(), &args.paths);
    let mut paths: Vec<PathBuf> = snapshot
        .path_index
        .visible_paths()
        .filter(|(_, path)| {
            matches_any_explicit_path(path, &explicit_specs)
                && matches_optional_glob(path, &args.file_types, &args.type_nots, &args.globs)
        })
        .map(|(_, path)| path.to_path_buf())
        .collect();
    paths.sort_unstable();
    paths
}
mod files;
pub(super) use files::cmd_files;

pub(super) fn sort_and_dedup_matches(
    mut matches: Vec<crate::SearchMatch>,
) -> Vec<crate::SearchMatch> {
    // Callers concatenate already-sorted per-spec runs, so a stable sort
    // (timsort) detects and merges those runs in ~O(n log k) rather than
    // re-sorting from scratch. `cmp_path_bytes` reproduces `Path::cmp` order.
    matches.sort_by(|a, b| {
        crate::path_util::cmp_path_bytes(&a.path, &b.path)
            .then_with(|| a.line_number.cmp(&b.line_number))
            .then_with(|| a.byte_offset.cmp(&b.byte_offset))
            .then_with(|| a.submatch_start.cmp(&b.submatch_start))
            .then_with(|| a.submatch_end.cmp(&b.submatch_end))
    });
    matches.dedup_by(|a, b| {
        a.path == b.path
            && a.line_number == b.line_number
            && a.byte_offset == b.byte_offset
            && a.submatch_start == b.submatch_start
            && a.submatch_end == b.submatch_end
    });
    matches
}

#[cfg(test)]
#[path = "../scope_tests.rs"]
mod tests;

