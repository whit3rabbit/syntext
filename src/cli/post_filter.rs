//! CLI-layer post-filtering of content-match result sets.
//!
//! Shared by the normal content-search path (`run_search`) and `--refs`
//! (find-references) so both honor the same scoping: `-t`/`-g` glob filters,
//! path-relative `--max-depth`, and per-file `-m` truncation. Lives apart from
//! `search.rs` to keep that file under the 400-line quality gate.

use super::scope::{
    matches_any_explicit_path, matches_optional_glob, path_depth, truncate_matches_per_file,
    ExplicitPathSpec,
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
        results.retain(|m| {
            matches_any_explicit_path(&m.path, explicit_specs)
                && matches_optional_glob(&m.path, &args.file_types, &args.type_nots, &args.globs)
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
                        spec.rel_path.as_os_str().is_empty()
                            || m.path.starts_with(&spec.rel_path)
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
