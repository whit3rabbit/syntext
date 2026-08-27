//! Shared result rendering and exit-code dispatch for content searches.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::search::MatchedFile;
use crate::Config;

use super::super::render;
use super::super::scope::collect_scoped_paths;
use super::SearchArgs;

/// Shared result rendering and exit-code dispatch for every content search
/// that already has its matches in hand: the indexed path passes
/// `Some(index)`, the stdin filter passes `None`. `stdin_scoped` is `Some(_)`
/// when a stdin input participates in this search (`Some(true)` = it comes
/// first in argv order); only the `-L` branch uses it, to list `<stdin>`
/// alongside the path inputs the way rg does.
pub(in crate::cli) fn render_results(
    config: &Config,
    index: Option<&Index>,
    results: Vec<crate::SearchMatch>,
    files: HashMap<PathBuf, MatchedFile>,
    output_args: &SearchArgs,
    elapsed: std::time::Duration,
    stdin_scoped: Option<bool>,
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
        // The stdin stream is an input like any other under -L: rg lists
        // `<stdin>` when the stream itself does not match, in argv position
        // among the path inputs for a mixed search.
        let mut scoped: Vec<PathBuf> = match index {
            Some(ix) => collect_scoped_paths(ix, config, output_args),
            None => Vec::new(),
        };
        match stdin_scoped {
            Some(true) => scoped.insert(0, PathBuf::from(super::super::stdin_search::STDIN_LABEL)),
            Some(false) => scoped.push(PathBuf::from(super::super::stdin_search::STDIN_LABEL)),
            None => {}
        }
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
        if let Err(err) =
            render::render_json(index, config, &results, &files, output_args, stdin_scoped)
        {
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
        return handle_output_code(render::render_count_matches(
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

    let render_call = if output_args.json {
        render::render_json(index, config, &results, &files, output_args, stdin_scoped)
    } else if output_args.vimgrep {
        render::render_vimgrep(config, &results, output_args)
    } else if output_args.only_matching {
        render::render_only_matching(config, &results, &files, output_args)
    } else if has_context {
        render::render_with_context(config, &results, &files, output_args)
    } else if output_args.heading {
        render::render_heading(&results, output_args)
    } else {
        render::render_flat(&results, output_args)
    };

    if let Err(err) = render_call {
        return handle_output(err);
    }

    0
}

pub(super) fn handle_output_code(result: io::Result<i32>) -> i32 {
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
