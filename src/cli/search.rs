//! Search argument parsing, query execution, and result rendering.

use std::collections::HashMap;

use std::path::PathBuf;
use std::time::Instant;

use crate::index::Index;
use crate::search::MatchedFile;
use crate::{Config, IndexError};

// Re-export for render submodules that import via `crate::cli::search::collect_scoped_paths`.
use super::post_filter::apply_post_filters;
use super::render::build_effective_pattern;
pub(super) use super::scope::collect_scoped_paths;
use super::scope::{explicit_path_specs, search_options, sort_and_dedup_matches};

pub(super) use super::search_args::SearchArgs;

mod output;
use output::handle_output_code;
pub(super) use output::render_results;

pub(super) fn cmd_search(config: Config, args: &SearchArgs) -> i32 {
    // Reject malformed -g/--glob specs before touching the index: a bad glob
    // otherwise degrades to a silent never-match filter (zero results, no error).
    if let Err((spec, msg)) = super::scope::validate_globs(&args.globs) {
        eprintln!("st: invalid glob '{spec}': {msg}");
        return 2;
    }

    // rg-style stdin filter: a pipe/redirect (or an explicit `-` path) is
    // searched in-memory before the index is ever opened, so it also works in
    // directories with no `.syntext` at all. A `-` mixed with real paths
    // searches BOTH: the stdin half is collected here, `-` is stripped from
    // the path arguments, and the halves are merged before rendering.
    let stdin_half = match super::stdin_search::run_or_collect_stdin(&config, args) {
        super::stdin_search::StdinFilterOutcome::Done(code) => return code,
        super::stdin_search::StdinFilterOutcome::Mixed(half) => Some(half),
        super::stdin_search::StdinFilterOutcome::NotStdin => None,
    };

    // Output defaults must be computed while `-` still counts as a search
    // input: rg labels BOTH halves of a mixed search (`<stdin>:...`,
    // `path:...`), so shows_filename_by_default has to see the two path
    // specs. Stripping `-` first (as the index half's run args require)
    // would suppress the filename prefix for both halves.
    let output_args = args.with_effective_output_defaults(&config);

    // Bounded LockConflict retry: `st` spawns its own background catch-up
    // writer, so a search can land in that child's exclusive window through no
    // fault of the caller. See `open_retry` -- every other error, corruption
    // included, still returns on the first attempt.
    let index = match super::open_retry::open_for_search(&config) {
        Ok(idx) => idx,
        // Only a missing index is eligible for fallback; a corrupt index or lock
        // conflict still fails loudly so we never mask real corruption. The
        // mixed-dash case cannot fall back: stdin was already consumed here,
        // and the fallback child would silently search an empty stream.
        Err(IndexError::IndexNotFound(dir)) if stdin_half.is_none() => {
            return super::fallback::handle_missing_index(&config, args, &dir);
        }
        Err(IndexError::IndexNotFound(dir)) => {
            eprintln!("st: no index found at {}", dir.display());
            eprintln!(
                "st:   build one with `st index`; the rg fallback cannot re-read stdin after '-'"
            );
            return 2;
        }
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };

    // rg reports each missing explicitly named path, still searches the
    // remaining inputs, and exits 2. Handling it here (after the fallback
    // decision, which reports missing paths itself via the rg child) keeps
    // every other input's output: the old early return dropped the surviving
    // paths' matches AND an already-collected stdin half.
    //
    // A missing path is intentionally left in `search_args.paths` (not
    // stripped): `explicit_path_specs`/`matches_any_explicit_path` treat a
    // nonexistent path as an explicit spec that matches nothing (the same way
    // `-L`'s `collect_scoped_paths` already does with the original, unfiltered
    // args), not as an absent scope. Stripping it here previously emptied the
    // path list whenever every named path was missing, which
    // `explicit_path_specs` reads as "no scope given" and silently falls back
    // to searching the whole repo -- leaking matches from files the caller
    // never named. See `tests/integration/cli.rs` for the regression case.
    let mut saw_missing_path = false;
    let search_args = args.clone();
    let missing = super::scope::missing_explicit_paths(&config.repo_root, &args.paths);
    if !missing.is_empty() {
        saw_missing_path = true;
        for path in &missing {
            eprintln!("st: {}: No such file or directory", path.display());
        }
    }

    // Bounded auto-update: run git change detection before searching so the
    // index is as fresh as possible within a latency budget, and emit the
    // staleness notice on stderr when still behind. See
    // `catchup::run_bounded_auto_update` for the full error-handling
    // contract (a failed or skipped update can only ever leave the index
    // stale, never change the search's own exit code).
    let needs_async_catchup = super::catchup::run_bounded_auto_update(&index, &config, args.quiet);

    let exit_code = match stdin_half {
        Some(half) => {
            // The index half must not see `-` as a path scope; strip it from
            // the run args only (output defaults above already accounted
            // for it).
            let mut run_args = search_args;
            run_args.paths.retain(|p| p.as_os_str() != "-");
            run_and_render(&index, &config, &run_args, &output_args, Some(half))
        }
        None => run_and_render(&index, &config, &search_args, &output_args, None),
    };

    // Spawn the async catch-up only after results have been printed, so the
    // extra process never delays or reorders the search's own stdout/stderr.
    if needs_async_catchup {
        super::catchup::maybe_spawn_async_catchup(&config);
    }

    // rg: an IO error on an explicitly named input beats the match/no-match
    // exit codes (2 over both 0 and 1).
    if saw_missing_path {
        return 2;
    }
    exit_code
}

fn run_and_render(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
    output_args: &SearchArgs,
    stdin_half: Option<super::stdin_search::StdinHalf>,
) -> i32 {
    #[cfg(feature = "symbols")]
    if args.sym.is_some() || args.refs.is_some() {
        // --sym and --refs are mutually exclusive; --sym-kind needs a name.
        if let Some(code) = super::sym::reject_sym_refs_conflicts(output_args) {
            return code;
        }
    }
    #[cfg(feature = "symbols")]
    if args.sym.is_some() {
        // --sym is a pure lookup: grep-style output modifiers do not apply.
        // (--refs produces content matches, so it skips this check.)
        if let Some(code) = super::sym::reject_incompatible_symbol_flags(output_args) {
            return code;
        }
    }

    let search_start = Instant::now();

    if output_args.invert_match {
        return handle_output_code(super::render::render_invert_match(
            index,
            config,
            output_args,
        ));
    }

    let outcome = match run_search(index, config, args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("st: {e}");
            return 2;
        }
    };
    let (mut results, mut files) = (outcome.matches, outcome.files);
    // Capture before the half is consumed below: -L lists the stdin input in
    // its argv position.
    let stdin_first = stdin_half.as_ref().map(|h| h.stdin_first);
    let mut trailing_notice: Option<u64> = None;
    let mut notice_printed = false;
    if let Some(half) = stdin_half {
        if let Some(offset) = half.binary_notice {
            // rg replaces a binary stdin half's line output with the
            // `binary file matches` notice, in this half's position.
            if half.stdin_first {
                let notice_code = super::stdin_search::print_binary_notice_exit_code(
                    offset,
                    output_args.no_filename,
                    output_args.vimgrep,
                );
                // A genuine write failure (not the always-0 broken-pipe
                // case) beats every other exit-code decision below, the same
                // way `render_results` short-circuits on its own io::Result.
                if notice_code != 0 {
                    return notice_code;
                }
                notice_printed = true;
            } else {
                trailing_notice = Some(offset);
            }
        } else {
            super::stdin_search::splice_stdin_half(half, &mut results, &mut files);
        }
    }
    let code = render_results(
        config,
        Some(index),
        results,
        files,
        output_args,
        search_start.elapsed(),
        stdin_first,
    );
    if let Some(offset) = trailing_notice {
        let notice_code = super::stdin_search::print_binary_notice_exit_code(
            offset,
            output_args.no_filename,
            output_args.vimgrep,
        );
        if notice_code != 0 {
            return notice_code;
        }
        notice_printed = true;
    }
    // A printed notice means the stdin half matched; rg exits 0 overall.
    if notice_printed && code == 1 {
        return 0;
    }
    code
}

pub(super) fn run_search(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
) -> Result<crate::search::SearchOutcome, crate::IndexError> {
    use crate::search::SearchOutcome;

    // Explicit symbol lookup (--sym). Bypasses content routing entirely; the flag
    // only exists when the symbols feature is built. No content map (renderers
    // fall back to disk reads for the rare symbol/refs case).
    #[cfg(feature = "symbols")]
    if let Some(name) = &args.sym {
        return Ok(SearchOutcome {
            matches: index.search_symbols(name, args.sym_kind.as_deref())?,
            files: HashMap::new(),
        });
    }
    // Find-references (--refs): resolve the name via the symbol index, then run
    // a word-boundary case-sensitive content search. Results are real content
    // matches, so the same -t/-g/--max-depth/-m post-filtering applies.
    #[cfg(feature = "symbols")]
    if let Some(name) = &args.refs {
        let explicit_specs = explicit_path_specs(&config.repo_root, &args.paths);
        let results = index.search_references(name, args.sym_kind.as_deref())?;
        return Ok(SearchOutcome {
            matches: apply_post_filters(results, args, &explicit_specs),
            files: HashMap::new(),
        });
    }
    let (routing_pattern, verify_pattern) = build_effective_pattern(args);
    let explicit_specs = explicit_path_specs(&config.repo_root, &args.paths);
    let make_opts = |path_filter: Option<String>| {
        let mut opts = search_options(args, path_filter);
        opts.verify_pattern = verify_pattern.clone();
        if args.count || args.files_with_matches || args.files_without_match {
            assert!(
                opts.max_results.is_none(),
                "max_results must be None in count/files-with-matches/files-without-match modes to avoid truncation bugs"
            );
        }
        opts
    };
    let (results, files): (Vec<crate::SearchMatch>, HashMap<PathBuf, MatchedFile>) =
        if explicit_specs.is_empty() {
            let out = index.search_with_content(&routing_pattern, &make_opts(None))?;
            (out.matches, out.files)
        } else {
            let mut merged = Vec::new();
            let mut files: HashMap<PathBuf, MatchedFile> = HashMap::new();
            for spec in &explicit_specs {
                let out = index
                    .search_with_content(&routing_pattern, &make_opts(Some(spec.path_filter())))?;
                merged.extend(out.matches);
                // First-wins: specs are normally disjoint scopes, and
                // sort_and_dedup_matches collapses any overlap; a file's content
                // is identical across specs within one snapshot generation.
                for (p, mf) in out.files {
                    files.entry(p).or_insert(mf);
                }
            }
            (sort_and_dedup_matches(merged), files)
        };
    Ok(SearchOutcome {
        matches: apply_post_filters(results, args, &explicit_specs),
        files,
    })
}
