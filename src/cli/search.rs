//! Search argument parsing, query execution, and result rendering.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::search::MatchedFile;
use crate::{Config, IndexError};

// Re-export for render submodules that import via `crate::cli::search::collect_scoped_paths`.
use super::post_filter::apply_post_filters;
use super::render::build_effective_pattern;
pub(super) use super::scope::collect_scoped_paths;
use super::scope::{explicit_path_specs, search_options, sort_and_dedup_matches};

pub(super) use super::search_args::SearchArgs;

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
    let mut args = args.clone();
    let stdin_half = match super::stdin_search::run_or_collect_stdin(&config, &args) {
        super::stdin_search::StdinFilterOutcome::Done(code) => return code,
        super::stdin_search::StdinFilterOutcome::Mixed(half) => {
            args.paths.retain(|p| p.as_os_str() != "-");
            Some(half)
        }
        super::stdin_search::StdinFilterOutcome::NotStdin => None,
    };

    let index = match Index::open(config.clone()) {
        Ok(idx) => idx,
        // Only a missing index is eligible for fallback; a corrupt index or lock
        // conflict still fails loudly so we never mask real corruption. The
        // mixed-dash case cannot fall back: stdin was already consumed here,
        // and the fallback child would silently search an empty stream.
        Err(IndexError::IndexNotFound(dir)) if stdin_half.is_none() => {
            return super::fallback::handle_missing_index(&config, &args, &dir);
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

    // Bounded auto-update: run git change detection before searching so the
    // index is as fresh as possible within a latency budget, and emit the
    // staleness notice on stderr when still behind. See
    // `catchup::run_bounded_auto_update` for the full error-handling
    // contract (a failed or skipped update can only ever leave the index
    // stale, never change the search's own exit code).
    let needs_async_catchup = super::catchup::run_bounded_auto_update(&index, &config, args.quiet);

    let exit_code = run_and_render(&index, &config, &args, stdin_half);

    // Spawn the async catch-up only after results have been printed, so the
    // extra process never delays or reorders the search's own stdout/stderr.
    if needs_async_catchup {
        super::catchup::maybe_spawn_async_catchup(&config);
    }

    exit_code
}

fn run_and_render(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
    stdin_half: Option<super::stdin_search::StdinHalf>,
) -> i32 {
    let output_args = args.with_effective_output_defaults(config);

    #[cfg(feature = "symbols")]
    if args.sym.is_some() || args.refs.is_some() {
        // --sym and --refs are mutually exclusive; --sym-kind needs a name.
        if let Some(code) = super::sym::reject_sym_refs_conflicts(&output_args) {
            return code;
        }
    }
    #[cfg(feature = "symbols")]
    if args.sym.is_some() {
        // --sym is a pure lookup: grep-style output modifiers do not apply.
        // (--refs produces content matches, so it skips this check.)
        if let Some(code) = super::sym::reject_incompatible_symbol_flags(&output_args) {
            return code;
        }
    }

    let search_start = Instant::now();

    if output_args.invert_match {
        return handle_output_code(super::render::render_invert_match(
            index,
            config,
            &output_args,
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
    let mut trailing_notice: Option<u64> = None;
    let mut notice_printed = false;
    if let Some(half) = stdin_half {
        if let Some(offset) = half.binary_notice {
            // rg replaces a binary stdin half's line output with the
            // `binary file matches` notice, in this half's position.
            if half.stdin_first {
                super::stdin_search::print_binary_notice(
                    offset,
                    output_args.no_filename,
                    output_args.vimgrep,
                );
                notice_printed = true;
            } else {
                trailing_notice = Some(offset);
            }
        } else if half.stdin_first {
            // rg processes `-` in argv position order; splice the stdin run
            // accordingly. Per-path runs stay consecutive either way, which
            // is what heading grouping and per-file truncation require.
            let mut merged = half.matches;
            merged.append(&mut results);
            results = merged;
        } else {
            results.extend(half.matches);
        }
        for (p, mf) in half.files {
            files.entry(p).or_insert(mf);
        }
    }
    let code = render_results(
        config,
        Some(index),
        results,
        files,
        &output_args,
        search_start.elapsed(),
    );
    if let Some(offset) = trailing_notice {
        super::stdin_search::print_binary_notice(
            offset,
            output_args.no_filename,
            output_args.vimgrep,
        );
        notice_printed = true;
    }
    // A printed notice means the stdin half matched; rg exits 0 overall.
    if notice_printed && code == 1 {
        return 0;
    }
    code
}

/// Shared result rendering and exit-code dispatch for every content search
/// that already has its matches in hand: the indexed path passes
/// `Some(index)`, the stdin filter passes `None`. Branches that genuinely
/// need the index (corpus `-L` listing) treat `None` as an empty scope, which
/// the stdin guard makes unreachable.
pub(super) fn render_results(
    config: &Config,
    index: Option<&Index>,
    results: Vec<crate::SearchMatch>,
    files: HashMap<PathBuf, MatchedFile>,
    output_args: &SearchArgs,
    elapsed: std::time::Duration,
) -> i32 {
    if output_args.search_stats {
        let matched_files: std::collections::BTreeSet<_> =
            results.iter().map(|m| &m.path).collect();
        eprintln!(
            "Elapsed: {:.6}s, Matches: {}, Files with matches: {}",
            elapsed.as_secs_f64(),
            results.len(),
            matched_files.len()
        );
    }

    if output_args.files_without_match {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let sep = if output_args.null { b'\0' } else { b'\n' };
        let matched: std::collections::BTreeSet<_> =
            results.iter().map(|m| m.path.clone()).collect();
        let mut found_any = false;
        let scoped: Vec<PathBuf> = match index {
            Some(ix) => collect_scoped_paths(ix, config, output_args),
            None => Vec::new(),
        };
        for path in scoped {
            if matched.contains(&path) {
                continue;
            }
            found_any = true;
            // Under -q, suppress output but keep scanning so the exit code
            // still reflects whether any unmatched file exists.
            if output_args.quiet {
                break;
            }
            let result = out
                .write_all(path_bytes(&path).as_ref())
                .and_then(|_| out.write_all(&[sep]));
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return if found_any { 0 } else { 1 };
    }

    if results.is_empty() && output_args.json {
        if let Err(err) = super::render::render_json(index, config, &results, &files, output_args) {
            return handle_output(err);
        }
        return 1;
    }

    if results.is_empty() {
        return 1;
    }

    if output_args.quiet {
        return 0;
    }

    if output_args.files_with_matches {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let sep = if output_args.null { b'\0' } else { b'\n' };
        let mut seen = std::collections::BTreeSet::new();
        for m in &results {
            seen.insert(m.path.clone());
        }
        for path in &seen {
            let result = out
                .write_all(path_bytes(path).as_ref())
                .and_then(|_| out.write_all(&[sep]));
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return 0;
    }

    if output_args.count_matches || (output_args.count && output_args.only_matching) {
        return handle_output_code(super::render::render_count_matches(
            config,
            &results,
            output_args,
        ));
    }

    if output_args.count {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let mut counts: std::collections::BTreeMap<PathBuf, usize> =
            std::collections::BTreeMap::new();
        for m in &results {
            *counts.entry(m.path.clone()).or_default() += 1;
        }
        for (path, n) in &counts {
            let result = if output_args.no_filename {
                writeln!(out, "{n}")
            } else {
                let count_sep = if output_args.null { b'\0' } else { b':' };
                out.write_all(path_bytes(path).as_ref())
                    .and_then(|_| out.write_all(&[count_sep]))
                    .and_then(|_| writeln!(out, "{n}"))
            };
            if let Err(err) = result {
                return handle_output(err);
            }
        }
        return 0;
    }

    let has_context = output_args.after_context > 0 || output_args.before_context > 0;

    let render = if output_args.json {
        super::render::render_json(index, config, &results, &files, output_args)
    } else if output_args.vimgrep {
        super::render::render_vimgrep(config, &results, output_args)
    } else if output_args.only_matching {
        super::render::render_only_matching(config, &results, &files, output_args)
    } else if has_context {
        super::render::render_with_context(config, &results, &files, output_args)
    } else if output_args.heading {
        super::render::render_heading(&results, output_args)
    } else {
        super::render::render_flat(&results, output_args)
    };

    if let Err(err) = render {
        return handle_output(err);
    }

    0
}

fn handle_output_code(result: io::Result<i32>) -> i32 {
    result.unwrap_or_else(handle_output)
}

fn handle_output(err: io::Error) -> i32 {
    if err.kind() == io::ErrorKind::BrokenPipe {
        0
    } else {
        eprintln!("st: {err}");
        2
    }
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
