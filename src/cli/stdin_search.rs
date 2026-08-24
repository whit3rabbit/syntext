//! rg-style stdin filter mode: search piped/redirected input without an index.
//!
//! `cat log | st 'pat'` previously ignored stdin and silently searched the
//! whole repo index (exit 0, wrong results). This module restores ripgrep's
//! filter contract: when stdin carries the search subject (pipe or file
//! redirect, or an explicit `-` path), the stream is searched in-memory and no
//! index is opened — so it also works in directories with no `.syntext` at
//! all.
//!
//! Implicit-stdin detection is deliberately conservative: only a FIFO (pipe)
//! or regular file (redirect) qualifies. A tty, socket, or `/dev/null` never
//! does, matching ripgrep and keeping `st pat` with no paths a repo search
//! even when stdin is not a terminal (agents' shells attach /dev/null or a
//! socket to stdin; treating those as "read stdin" would silently search an
//! empty stream and exit 1).

use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::render;
use super::search::{render_results, SearchArgs};
use crate::search::verifier::verify_regex;

/// Label ripgrep uses for matches that came from stdin.
pub(super) const STDIN_LABEL: &str = "<stdin>";

/// Outcome of the stdin-mode guard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum StdinDecision {
    /// Not a stdin search; continue on the normal index path.
    NotStdin,
    /// Read and search stdin.
    UseStdin,
    /// `-` was combined with other path arguments: search both. The stdin
    /// half is collected here; the paths half runs on the index path.
    StdinPlusPaths,
}

/// Decide whether this invocation searches stdin. Pure over
/// (`stdin_searchable`, `args`) so the routing table is unit-testable without
/// touching real file descriptors.
pub(super) fn decide_stdin(stdin_searchable: bool, args: &SearchArgs) -> StdinDecision {
    // No pattern (or a symbols lookup) is never a content filter.
    if args.pattern.is_empty() || args.files_without_match {
        return StdinDecision::NotStdin;
    }
    // --sym/--refs route through the symbol index; they need the index.
    if args.sym.is_some() || args.refs.is_some() {
        return StdinDecision::NotStdin;
    }
    // An explicit `-` always means stdin, whatever stdin is attached to
    // (ripgrep reads stdin for `-` even when it is /dev/null).
    if args.paths.iter().any(|p| p.as_os_str() == "-") {
        return if args.paths.len() == 1 {
            StdinDecision::UseStdin
        } else {
            StdinDecision::StdinPlusPaths
        };
    }
    // Explicit real paths win over stdin (ripgrep rule).
    if !args.paths.is_empty() {
        return StdinDecision::NotStdin;
    }
    // Implicit stdin needs the CLI process boundary's blessing: in-process
    // `cmd_search` callers must not inherit stdin-mode behavior from however
    // their own process was launched (see SearchArgs::allow_implicit_stdin).
    if args.allow_implicit_stdin && stdin_searchable {
        StdinDecision::UseStdin
    } else {
        StdinDecision::NotStdin
    }
}

/// True when stdin is a pipe (FIFO) or a regular-file redirect — the only
/// shapes that carry an implicit search subject. Everything else (tty, socket,
/// /dev/null char device, closed fd, stat failure) stays on the repo path.
#[cfg(unix)]
fn stdin_is_searchable() -> bool {
    use std::os::unix::fs::FileTypeExt;
    if std::io::stdin().is_terminal() {
        return false;
    }
    match std::fs::metadata("/dev/stdin") {
        Ok(md) => md.file_type().is_fifo() || md.file_type().is_file(),
        // Fail safe, not open: an unresolvable stdin (rarely, macOS fdesc
        // stat races under heavy process churn return EBADF) keeps the
        // repo-index path, never silently filters an empty stream.
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn stdin_is_searchable() -> bool {
    // No /dev/stdin to stat on Windows; only the explicit `-` form engages.
    false
}

/// One half of a mixed `-` + paths search: the matches collected from stdin.
pub(super) struct StdinHalf {
    pub(super) matches: Vec<crate::SearchMatch>,
    pub(super) files: HashMap<PathBuf, crate::search::MatchedFile>,
    /// `-` preceded every real path in argv order; rg prints stdin results
    /// before file results in that case (and after otherwise).
    pub(super) stdin_first: bool,
}

/// What `cmd_search` should do after the stdin guard runs.
pub(super) enum StdinFilterOutcome {
    /// Not a stdin search; continue on the normal index path.
    NotStdin,
    /// The stdin search ran and rendered; exit with this code.
    Done(i32),
    /// `-` mixed with real paths: the stdin half is collected, and the caller
    /// must strip `-` from the path arguments and merge before rendering.
    Mixed(StdinHalf),
}

/// Entry point from `cmd_search`.
pub(super) fn run_or_collect_stdin(
    config: &crate::Config,
    args: &SearchArgs,
) -> StdinFilterOutcome {
    match decide_stdin(stdin_is_searchable(), args) {
        StdinDecision::NotStdin => StdinFilterOutcome::NotStdin,
        StdinDecision::StdinPlusPaths => {
            // stdin -v inverts per-line while indexed -v lists non-matching
            // files corpus-wide; the two cannot be merged into one output.
            if args.invert_match {
                eprintln!(
                    "st: '-' (stdin) cannot be combined with other paths under -v (stdin inverts per-line; files invert corpus-wide)"
                );
                return StdinFilterOutcome::Done(2);
            }
            match collect_stdin(args) {
                Ok((matches, files)) => StdinFilterOutcome::Mixed(StdinHalf {
                    matches,
                    files,
                    stdin_first: dash_precedes_real_paths(args),
                }),
                Err(code) => StdinFilterOutcome::Done(code),
            }
        }
        StdinDecision::UseStdin => StdinFilterOutcome::Done(run_stdin_filter(config, args)),
    }
}

/// True when the first `-` path argument precedes the first real path (rg
/// processes `-` in argv position order).
fn dash_precedes_real_paths(args: &SearchArgs) -> bool {
    let first_dash = args.paths.iter().position(|p| p.as_os_str() == "-");
    let first_real = args.paths.iter().position(|p| p.as_os_str() != "-");
    match (first_dash, first_real) {
        (Some(d), Some(r)) => d < r,
        _ => true,
    }
}

/// Read stdin and produce its match half. `Err` carries the process exit
/// code for read/regex failures.
fn collect_stdin(
    args: &SearchArgs,
) -> Result<
    (
        Vec<crate::SearchMatch>,
        HashMap<PathBuf, crate::search::MatchedFile>,
    ),
    i32,
> {
    let mut raw = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut raw) {
        eprintln!("st: failed to read stdin: {e}");
        return Err(2);
    }
    let raw_len = raw.len() as u64;
    let content = crate::index::normalize_encoding(&raw).into_owned();

    // Path filters can never match a stdin stream (`<stdin>` is not a repo
    // path); applying them would silently drop every match. Warn and strip,
    // keeping only the content-level post-filters (-m, --max-columns).
    let mut filter_args = args.clone();
    if !filter_args.globs.is_empty()
        || !filter_args.file_types.is_empty()
        || !filter_args.type_nots.is_empty()
        || filter_args.max_depth.is_some()
    {
        eprintln!(
            "st: -g/--include/--exclude/--exclude-dir/-t/-T/--max-depth are ignored when reading stdin"
        );
    }
    filter_args.globs.clear();
    filter_args.file_types.clear();
    filter_args.type_nots.clear();
    filter_args.max_depth = None;

    let re = match render::compile_output_regex(&filter_args) {
        Ok(re) => re,
        Err(e) => {
            eprintln!("st: invalid pattern: {e}");
            return Err(2);
        }
    };
    let label = PathBuf::from(STDIN_LABEL);
    let matches = if filter_args.invert_match {
        invert_matches(&re, &label, &content)
    } else {
        verify_regex(&re, &label, &content, false)
    };
    let matches = super::post_filter::apply_post_filters(matches, &filter_args, &[]);

    let mut files = HashMap::new();
    files.insert(
        label,
        crate::search::MatchedFile {
            normalized: content.into(),
            raw_len,
        },
    );
    Ok((matches, files))
}

fn run_stdin_filter(config: &crate::Config, args: &SearchArgs) -> i32 {
    let start = Instant::now();
    let (matches, files) = match collect_stdin(args) {
        Ok(v) => v,
        Err(code) => return code,
    };

    // ripgrep's single-input rule: no filename prefix unless -H asks for one.
    let mut output_args = args.with_effective_output_defaults(config);
    if !output_args.with_filename {
        output_args.no_filename = true;
    }
    render_results(config, None, matches, files, &output_args, start.elapsed())
}

/// Per-line invert for stdin streams. st's indexed `-v` is corpus-wide (list
/// non-matching files), which is meaningless for a single stream; a piped
/// `st -v` therefore inverts line-by-line, like `rg -v` in a pipe.
fn invert_matches(
    re: &regex::bytes::Regex,
    path: &Path,
    content: &[u8],
) -> Vec<crate::SearchMatch> {
    if crate::index::walk::is_binary(content) {
        return Vec::new();
    }
    let mut out = Vec::new();
    crate::search::lines::for_each_line(content, |line_num, line_start, line| {
        if !re.is_match(line) {
            out.push(crate::SearchMatch {
                path: path.to_path_buf(),
                line_number: line_num,
                // rg -v prints the raw line, `\r` included.
                line_content: render::rendered_line(content, line_start, line).to_vec(),
                // No submatch exists on an inverted line; report the line's
                // own offset so -b/--byte-offset stays truthful.
                byte_offset: line_start as u64,
                submatch_start: 0,
                submatch_end: 0,
            });
        }
    });
    out
}

#[cfg(test)]
#[path = "stdin_search_tests.rs"]
mod tests;
