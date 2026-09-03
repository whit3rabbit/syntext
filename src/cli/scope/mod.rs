//! Path-scope filtering helpers: CLI path resolution, glob matching, explicit
//! path specs, file enumeration (--files mode), and result deduplication.

use std::path::{Path, PathBuf};

use crate::index::Index;
use crate::path::filter::matches_path_filter;
use crate::{Config, SearchOptions};

use super::search::SearchArgs;

mod resolve;
pub(super) use resolve::missing_explicit_paths;
use resolve::{path_is_directory, relativize_cli_path};

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
    // "-" (stdin) never produces an `explicit_path_specs` entry (it is not a
    // filesystem path), so counting specs alone silently drops it from the
    // input count. That let a mixed `-` + "." search (or any real path whose
    // `rel_path` normalizes to empty, like the repo root) collapse to a
    // single spec -- "-"'s, since "." is filtered out as an empty rel_path --
    // and get misread as "one plain file", hiding the filename prefix rg
    // shows on both halves of a mixed search. A lone `-` is the one true
    // single-input case (no filename); combined with anything else (another
    // `-`, or a real path) it is always multi-input, per rg.
    if paths.iter().any(|p| p.as_os_str() == "-") {
        return paths.len() > 1;
    }
    match explicit_path_specs(config.repo_root.as_path(), paths).as_slice() {
        [] => true,
        [spec] => spec.is_dir,
        _ => true,
    }
}

/// Whether `--max-results N` can be pushed down into the library as an
/// early-exit budget, and at what value.
///
/// The library stops resolving and verifying once it has `max_results`
/// matches, which is the whole performance win. That is only safe when no
/// later stage can *drop* matches: a CLI post-filter (`-t`, `-g`, `-T`,
/// `--max-depth`, `-m`) applied after an early exit would leave fewer than N
/// results even though more existed. A per-spec `path_filter` is the same
/// hazard from the other direction, since each of several explicit path specs
/// would get its own budget and the merged set would be capped at N per spec.
///
/// `-l` also opts out: the cap counts distinct files there, and the library
/// counts matches, so N matches can be far fewer than N files. `-c`,
/// `--count-matches`, `-v`, and `-L` never reach here (rejected up front),
/// and the assertion in `run_search` still guards that.
///
/// `N + 1` rather than `N`, so `apply_max_results` can tell "exactly N" from
/// "N and more were available" and only then print the truncation notice.
fn library_max_results(args: &SearchArgs, has_path_filter: bool) -> Option<usize> {
    let limit = args.max_results?;
    let post_filtered = has_path_filter
        || !args.file_types.is_empty()
        || !args.type_nots.is_empty()
        || !args.globs.is_empty()
        || args.max_depth.is_some()
        || args.max_count.is_some();
    if post_filtered
        || args.files_with_matches
        || args.files_without_match
        || args.count
        || args.count_matches
        || args.invert_match
    {
        return None;
    }
    limit.checked_add(1)
}

pub(super) fn search_options(args: &SearchArgs, path_filter: Option<String>) -> SearchOptions {
    SearchOptions {
        case_insensitive: args.ignore_case,
        // Pass every -t/-T through the multi-type vecs so the path index
        // narrows even for `-t rs -t py` (the singles stay unused here).
        file_type: None,
        exclude_type: None,
        file_types: args.file_types.clone(),
        exclude_types: args.type_nots.clone(),
        max_results: library_max_results(args, path_filter.is_some()),
        path_filter,
        verify_pattern: None,
        // -l/-L only need which files matched, never the line bytes. -c is
        // excluded: count re-scans line_content for per-line occurrences.
        skip_line_content: args.files_with_matches || args.files_without_match,
        deterministic: false,
        #[cfg(any(test, feature = "oracle"))]
        force_full_scan: false,
    }
}

/// Compile one `-g`/`--glob` pattern. A pattern containing '/' treats slashes
/// as real component boundaries (`literal_separator(true)`); a basename pattern
/// does not. Single source of truth for both `validate_globs` (up-front error
/// reporting) and `matches_optional_glob` (matching), so validation can never
/// drift from the compile that actually runs.
fn compile_glob(pattern: &str) -> Result<globset::Glob, globset::Error> {
    use globset::GlobBuilder;
    if pattern.contains('/') {
        GlobBuilder::new(pattern).literal_separator(true).build()
    } else {
        GlobBuilder::new(pattern).build()
    }
}

/// Precompiled `-g`/`--glob` specs: each pattern is parsed once into a
/// `GlobSet` (no per-path recompilation), tagged exclude-vs-include and
/// basename-vs-full-path, in CLI order for last-match-wins. Build once per
/// search via [`CompiledGlobs::build`], then reuse across every candidate path.
///
/// `GlobSet` is used (not `GlobMatcher`) because its `MatchStrategy` system
/// classifies patterns like `src/` as `Prefix`, correctly matching paths that
/// *start with* the prefix. `GlobMatcher` compiles to a regex like `^src/$`
/// which only matches the exact literal — a regression in prefix/suffix
/// matching semantics.
pub(super) struct CompiledGlobs {
    entries: Vec<GlobEntry>,
    /// True if any positive (non-`!`) glob is present. Drives the no-match
    /// fallback: a set of only excludes includes everything not excluded.
    has_positive: bool,
}

struct GlobEntry {
    is_exclude: bool,
    /// Pattern had no '/': match against the basename only (rg `-g` semantics).
    basename_only: bool,
    set: globset::GlobSet,
}

impl CompiledGlobs {
    /// Compile each spec once, skipping malformed globs (mirrors the old inline
    /// `let Ok(glob) = compile_glob(..) else continue`). Up-front rejection of
    /// bad specs is [`validate_globs`]'s job; callers run it first and exit 2,
    /// so by the time we build here every surviving spec compiles.
    pub(super) fn build(path_globs: &[String]) -> Self {
        let mut entries = Vec::new();
        let mut has_positive = false;
        for glob_str in path_globs {
            let (is_exclude, pattern) = match glob_str.strip_prefix('!') {
                Some(excl) => (true, excl),
                None => (false, glob_str.as_str()),
            };
            if pattern.is_empty() {
                continue;
            }
            if !is_exclude {
                has_positive = true;
            }
            let Ok(glob) = compile_glob(pattern) else {
                continue;
            };
            let mut builder = globset::GlobSetBuilder::new();
            builder.add(glob);
            let Ok(set) = builder.build() else {
                continue;
            };
            entries.push(GlobEntry {
                is_exclude,
                basename_only: !pattern.contains('/'),
                set,
            });
        }
        Self {
            entries,
            has_positive,
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub(super) fn matches_optional_glob(
    path: &Path,
    file_types: &[String],
    exclude_types: &[String],
    globs: &CompiledGlobs,
) -> bool {
    if !file_types.is_empty()
        && !file_types
            .iter()
            .any(|file_type| matches_path_filter(path, &[file_type.as_str()], &[], None))
    {
        return false;
    }

    if exclude_types
        .iter()
        // "does the path HAVE this excluded extension" — pass as an include
        // probe; a true result means the path carries an excluded type.
        .any(|exclude_type| matches_path_filter(path, &[exclude_type.as_str()], &[], None))
    {
        return false;
    }

    if globs.is_empty() {
        return true;
    }

    // globset semantics matching rg's -g behaviour:
    //
    //   • Patterns WITHOUT '/' match the **basename** only, so `*.rs` matches
    //     `src/lib.rs` (compiled without literal_separator, tested against
    //     Path::file_name()).
    //   • Patterns WITH '/' match the **full relative path** with
    //     literal_separator(true), so `src/foo` does NOT substring-match
    //     `mysrc/foo` (each slash is a component boundary).
    //
    // Last matching glob wins (exclude vs include); a set of only excludes
    // includes everything not excluded.
    let basename = path.file_name().map(Path::new);
    let mut state: Option<bool> = None; // None means Undecided
    for entry in &globs.entries {
        let matches = if entry.basename_only {
            basename.is_some_and(|b| entry.set.is_match(b))
        } else {
            entry.set.is_match(path)
        };
        if matches {
            state = Some(!entry.is_exclude);
        }
    }
    match state {
        Some(included) => included,
        None => !globs.has_positive,
    }
}

/// Validate `-g`/`--glob` specs up front via the shared `compile_glob`, the
/// same compile `matches_optional_glob` runs. Returns `Err((spec, message))` on
/// the first glob that fails to build. Without this, `matches_optional_glob`
/// silently swallows a malformed positive glob into a never-matches filter, so
/// a typo like `-g '[bad'` returns zero results with no error -- the worst
/// failure mode for an agent. Call once before searching and exit 2 on error.
pub(super) fn validate_globs(path_globs: &[String]) -> Result<(), (String, String)> {
    for glob_str in path_globs {
        let pattern = glob_str.strip_prefix('!').unwrap_or(glob_str.as_str());
        if pattern.is_empty() {
            continue;
        }
        if let Err(e) = compile_glob(pattern) {
            return Err((glob_str.clone(), e.to_string()));
        }
    }
    Ok(())
}

pub(super) fn collect_scoped_paths(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
) -> Vec<PathBuf> {
    let snapshot = index.snapshot();
    let explicit_specs = explicit_path_specs(config.repo_root.as_path(), &args.paths);
    // Compile globs once, not once per candidate path.
    let compiled_globs = CompiledGlobs::build(&args.globs);
    let mut paths: Vec<PathBuf> = snapshot
        .path_index
        .visible_paths()
        .filter(|(_, path)| {
            matches_any_explicit_path(path, &explicit_specs)
                && matches_optional_glob(path, &args.file_types, &args.type_nots, &compiled_globs)
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
