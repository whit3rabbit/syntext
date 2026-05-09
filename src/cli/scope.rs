//! Path-scope filtering helpers: CLI path resolution, glob matching, explicit
//! path specs, file enumeration (--files mode), and result deduplication.

use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::index::Index;
use crate::path::filter::{matches_path_filter, path_matches_glob};
use crate::path_util::path_bytes;
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
    rel_path: PathBuf,
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
    paths
        .iter()
        .map(|path| ExplicitPathSpec {
            rel_path: relativize_cli_path(repo_root, path),
            is_dir: path_is_directory(repo_root, path),
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

fn path_is_directory(repo_root: &Path, path: &Path) -> bool {
    cli_path_on_disk(repo_root, path)
        .metadata()
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

fn relativize_cli_path(repo_root: &Path, path: &Path) -> PathBuf {
    let rel = if path.is_absolute() {
        path.strip_prefix(repo_root).unwrap_or(path)
    } else {
        path
    };
    crate::path_util::normalize_to_forward_slashes(normalize_relative_path(rel))
}

fn cli_path_on_disk(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
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

    let mut has_positive_glob = false;
    let mut matched_positive_glob = false;
    for glob in path_globs {
        if let Some(exclude_glob) = glob.strip_prefix('!') {
            if !exclude_glob.is_empty() && path_matches_glob(path, exclude_glob) {
                return false;
            }
            continue;
        }
        has_positive_glob = true;
        matched_positive_glob |= path_matches_glob(path, glob);
    }

    !has_positive_glob || matched_positive_glob
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

/// List indexed files matching type/glob filters (--files mode).
pub(super) fn cmd_files(config: Config, cli: &super::args::Cli) -> i32 {
    let index = match crate::index::Index::open(config.clone()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };
    let snapshot = index.snapshot();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let sep = if cli.null { b'\0' } else { b'\n' };
    let globs = cli.combined_globs();
    let mut paths: Vec<_> = snapshot
        .path_index
        .visible_paths()
        .filter(|(_, path)| matches_optional_glob(path, &cli.file_type, &cli.type_not, &globs))
        .map(|(_, path)| path.to_path_buf())
        .collect();
    if let Some(depth) = cli.max_depth {
        paths.retain(|p| path_depth(p) <= depth);
    }
    paths.sort_unstable();
    for path in &paths {
        let result = out
            .write_all(path_bytes(path).as_ref())
            .and_then(|_| out.write_all(&[sep]));
        if let Err(err) = result {
            if err.kind() == io::ErrorKind::BrokenPipe {
                return 0;
            }
            eprintln!("st: {err}");
            return 2;
        }
    }
    0
}

pub(super) fn sort_and_dedup_matches(
    mut matches: Vec<crate::SearchMatch>,
) -> Vec<crate::SearchMatch> {
    matches.sort_unstable_by(|a, b| {
        a.path
            .cmp(&b.path)
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
