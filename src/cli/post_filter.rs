//! CLI-layer post-filtering of content-match result sets.
//!
//! Shared by the normal content-search path (`run_search`) and `--refs`
//! (find-references) so both honor the same scoping: `-t`/`-g` glob filters,
//! path-relative `--max-depth`, and per-file `-m` truncation. Lives apart from
//! `search.rs` to keep that file under the 400-line quality gate.

use super::scope::{
    matches_any_explicit_path, matches_optional_glob, path_depth, truncate_matches_per_file,
    CompiledGlobs, ExplicitPathSpec,
};
use super::search::SearchArgs;

/// Apply `-t`/`-g`/`--max-depth`/`-m` post-filtering to a result set.
pub(super) fn apply_post_filters(
    mut results: Vec<crate::SearchMatch>,
    args: &SearchArgs,
    explicit_specs: &[ExplicitPathSpec],
) -> Vec<crate::SearchMatch> {
    if !explicit_specs.is_empty()
        || !args.file_types.is_empty()
        || !args.type_nots.is_empty()
        || !args.globs.is_empty()
    {
        let compiled_globs = CompiledGlobs::build(&args.globs);
        results.retain(|m| {
            matches_any_explicit_path(&m.path, explicit_specs)
                && matches_optional_glob(
                    &m.path,
                    &args.file_types,
                    &args.type_nots,
                    &compiled_globs,
                )
        });
    }
    if let Some(depth) = args.max_depth {
        // rg counts depth relative to each search path argument, not the repo
        // root. `st pat src --max-depth 1` keeps `src/foo.rs` (depth 1 inside
        // `src`) and drops `src/a/b.rs` (depth 2). With no explicit paths the
        // repo root is the search root, so repo-root-relative path_depth() is
        // correct.
        if explicit_specs.is_empty() {
            results.retain(|m| path_depth(&m.path) <= depth);
        } else {
            results.retain(|m| {
                // Deepest spec root that is a prefix of this path = most specific.
                let spec_depth = explicit_specs
                    .iter()
                    .filter(|spec| {
                        spec.rel_path.as_os_str().is_empty() || m.path.starts_with(&spec.rel_path)
                    })
                    .map(|spec| spec.rel_path.components().count())
                    .max()
                    .unwrap_or(0);
                path_depth(&m.path).saturating_sub(spec_depth) <= depth
            });
        }
    }
    if let Some(limit) = args.max_count {
        results = truncate_matches_per_file(results, limit);
    }
    results
}

/// Apply the total output cap (`--max-results`) to a final result set, and
/// report whether anything was dropped.
///
/// Runs last, after `apply_post_filters` and after any stdin half has been
/// spliced in, so it bounds what is actually printed rather than what the
/// index half happened to produce. Detection is exact: `search_options` asks
/// the library for `limit + 1` results when it can, so a set that still has
/// more than `limit` entries here really did have more to give.
///
/// `-l` prints one line per distinct path, so under `-l` the cap counts
/// distinct paths. Results arrive sorted by path, so "the first N paths" is
/// well defined. Modes whose output is not derived from this vector at all
/// (`-c`, `--count-matches`, `-v`, `-L`, `--files`) are rejected up front
/// rather than silently ignored.
pub(super) fn apply_max_results(results: &mut Vec<crate::SearchMatch>, args: &SearchArgs) -> bool {
    let Some(limit) = args.max_results else {
        return false;
    };
    if args.files_with_matches {
        // Mirror what -l prints: the distinct paths, in sorted order. Keeping
        // the first `limit` of *that* set (rather than the first `limit`
        // entries of `results`) is what makes the cut deterministic even when
        // a stdin half has been spliced in out of path order.
        let distinct: std::collections::BTreeSet<&std::path::Path> =
            results.iter().map(|m| m.path.as_path()).collect();
        let truncated = distinct.len() > limit;
        let keep: std::collections::BTreeSet<std::path::PathBuf> = distinct
            .into_iter()
            .take(limit)
            .map(|p| p.to_path_buf())
            .collect();
        results.retain(|m| keep.contains(&m.path));
        return truncated;
    }
    let truncated = results.len() > limit;
    results.truncate(limit);
    truncated
}

/// Reject `--max-results` in the output modes whose printed lines are not
/// this vector's entries. Returns `Some(exit_code)` after reporting on stderr.
pub(super) fn reject_max_results_conflicts(args: &SearchArgs) -> Option<i32> {
    args.max_results?;
    // (is-set, flag). `-l` is deliberately absent: it caps distinct files.
    let checks: [(bool, &str); 4] = [
        (args.count, "-c/--count"),
        (args.count_matches, "--count-matches"),
        (args.invert_match, "-v/--invert-match"),
        (args.files_without_match, "-L/--files-without-match"),
    ];
    for (is_set, flag) in checks {
        if is_set {
            eprintln!("st: --max-results is not supported with {flag}");
            return Some(2);
        }
    }
    None
}
