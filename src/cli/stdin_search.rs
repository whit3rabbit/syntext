//! rg-style stdin filter mode: search piped/redirected input without an index.
//!
//! `cat log | st 'pat'` previously ignored stdin and silently searched the
//! whole repo index (exit 0, wrong results). This module restores ripgrep's
//! filter contract: when stdin carries the search subject (pipe or file
//! redirect, or an explicit `-` path), the stream is searched in-memory and no
//! index is opened, so it also works in directories with no `.syntext` at
//! all.
//!
//! Implicit-stdin detection is deliberately conservative: only a FIFO (pipe)
//! or regular file (redirect) qualifies. A tty, socket, or `/dev/null` never
//! does, matching ripgrep and keeping `st pat` with no paths a repo search
//! even when stdin is not a terminal (agents' shells attach /dev/null or a
//! socket to stdin; treating those as "read stdin" would silently search an
//! empty stream and exit 1).

use std::collections::HashMap;
// The unix stdin probe uses the file-level import; the Windows one has its
// own local `use` (it also needs the windows os trait), so gate this one or
// the non-unix build warns about an unused import.
#[cfg(unix)]
use std::io::IsTerminal;
use std::io::Read;
use std::path::PathBuf;

use super::render;
use super::search::SearchArgs;
use crate::search::verifier::verify_regex;
use run::{invert_matches, render_stdin_half};

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
    // --sym/--refs route through the symbol index; they need the index. (A `-`
    // combined with them is rejected by the caller before this runs.)
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
    // An empty pattern still filters the stream (rg `cmd | rg ''` prints
    // every line), and so does -L (rg --files-without-match lists `<stdin>`
    // when the stream does not match): neither guards the implicit path.
    if args.allow_implicit_stdin && stdin_searchable {
        StdinDecision::UseStdin
    } else {
        StdinDecision::NotStdin
    }
}

/// True when `fd` is a pipe (FIFO) or a regular file, the only shapes that
/// carry an implicit search subject. Everything else (tty, socket, /dev/null
/// char device, closed fd, stat failure) stays on the repo path.
///
/// Classifies the descriptor itself (`fstat` on a dup), never a path. Stating
/// `/dev/stdin` instead goes through macOS's `fdesc` filesystem, whose lookup
/// re-resolves fd 0 out of the current process's descriptor table and
/// transiently returns EBADF under heavy process churn: measured at ~0.2% of
/// invocations on a loaded 14-core machine with a real pipe on fd 0, versus 0
/// failures in 8000 `fstat` probes under identical load. That misclassified a
/// piped stream as "not stdin" and silently searched the repo index instead.
#[cfg(unix)]
fn fd_is_searchable(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    use std::os::unix::fs::FileTypeExt;
    // `try_clone_to_owned` is F_DUPFD_CLOEXEC; the File owns only the dup, so
    // dropping it never closes the caller's descriptor.
    let Ok(owned) = fd.try_clone_to_owned() else {
        return false;
    };
    match std::fs::File::from(owned).metadata() {
        Ok(md) => md.file_type().is_fifo() || md.file_type().is_file(),
        // Fail safe, not open: an unclassifiable stdin keeps the repo-index
        // path, never silently filters an empty stream.
        Err(_) => false,
    }
}

#[cfg(unix)]
fn stdin_is_searchable() -> bool {
    use std::os::fd::AsFd;
    if std::io::stdin().is_terminal() {
        return false;
    }
    fd_is_searchable(std::io::stdin().as_fd())
}

#[cfg(not(unix))]
fn stdin_is_searchable() -> bool {
    // No /dev/stdin to stat on Windows; classify the stdin handle itself via
    // GetFileType, the same call std uses under the hood for tty detection.
    // DISK = regular-file redirect, PIPE = shell pipe; CHAR covers the
    // console (already excluded above) and NUL, and UNKNOWN stays on the
    // repo path so an odd handle never silently filters an empty stream.
    use std::io::IsTerminal;
    use std::os::windows::io::AsRawHandle;

    // Safety: GetFileType only reads a type tag for our own stdin handle; it
    // writes nothing, keeps no pointer, and the handle outlives the call.
    // Declared inline because the crate has no windows-sys dependency.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileType(h_file: *mut core::ffi::c_void) -> u32;
    }
    const FILE_TYPE_DISK: u32 = 1;
    const FILE_TYPE_PIPE: u32 = 3;

    if std::io::stdin().is_terminal() {
        return false;
    }
    // SAFETY: see the declaration above.
    let kind = unsafe { GetFileType(std::io::stdin().as_raw_handle().cast()) };
    kind == FILE_TYPE_DISK || kind == FILE_TYPE_PIPE
}

/// One half of a mixed `-` + paths search: the matches collected from stdin.
pub(super) struct StdinHalf {
    pub(super) matches: Vec<crate::SearchMatch>,
    pub(super) files: HashMap<PathBuf, crate::search::MatchedFile>,
    /// `-` preceded every real path in argv order; rg prints stdin results
    /// before file results in that case (and after otherwise).
    pub(super) stdin_first: bool,
    /// When the stream is binary (NUL) and line output was suppressed in
    /// favor of rg's `binary file matches` notice: the offset of the first
    /// NUL byte. The caller prints the notice in this half's position and
    /// must not print the (cleared) line matches.
    pub(super) binary_notice: Option<u64>,
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
    // Symbol search cannot read a stream; an explicit `-` next to --sym/--refs
    // would otherwise survive as a bogus repo-relative path named `-` and
    // silently discard the stdin half the user asked for.
    if (args.sym.is_some() || args.refs.is_some())
        && args.paths.iter().any(|p| p.as_os_str() == "-")
    {
        eprintln!(
            "st: '-' (stdin) cannot be combined with --sym/--refs (symbol search reads the index, not a stream)"
        );
        return StdinFilterOutcome::Done(2);
    }
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
            // `st pat - -`: every path argument is a dash, so after they are
            // stripped the index half would have NO scope and silently search
            // the whole repo. rg reads the stream once (a later `-` just sees
            // EOF) and searches nothing else; render the stream with the
            // multi-input naming rules the dash list implies.
            let all_dashes = args.paths.iter().all(|p| p.as_os_str() == "-");
            let start = std::time::Instant::now();
            match collect_stdin(args) {
                Ok((matches, files, binary_notice)) => {
                    if all_dashes {
                        return StdinFilterOutcome::Done(render_stdin_half(
                            config,
                            args,
                            matches,
                            files,
                            binary_notice,
                            false,
                            start.elapsed(),
                        ));
                    }
                    StdinFilterOutcome::Mixed(StdinHalf {
                        matches,
                        files,
                        stdin_first: dash_precedes_real_paths(args),
                        binary_notice,
                    })
                }
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

/// Splice a collected stdin half into the index results. rg processes `-` in
/// argv position order; per-path runs stay consecutive either way, which is
/// what heading grouping and per-file truncation require.
pub(super) fn splice_stdin_half(
    half: StdinHalf,
    results: &mut Vec<crate::SearchMatch>,
    files: &mut HashMap<PathBuf, crate::search::MatchedFile>,
) {
    if half.stdin_first {
        let mut merged = half.matches;
        merged.append(results);
        *results = merged;
    } else {
        results.extend(half.matches);
    }
    for (p, mf) in half.files {
        files.entry(p).or_insert(mf);
    }
}

/// The collected stdin half of a search: line matches, the `<stdin>` file
/// entry for renderers that re-read content, and (when rg's binary policy
/// suppressed the line output) the offset of the first NUL byte.
type StdinCollect = (
    Vec<crate::SearchMatch>,
    HashMap<PathBuf, crate::search::MatchedFile>,
    Option<u64>,
);

/// Read stdin and produce its match half. `Err` carries the process exit
/// code for read/regex failures.
fn collect_stdin(args: &SearchArgs) -> Result<StdinCollect, i32> {
    let mut raw = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut raw) {
        eprintln!("st: failed to read stdin: {e}");
        return Err(2);
    }
    let raw_len = raw.len() as u64;
    // Take the owned conversion only when normalization actually rewrote
    // something; `.into_owned()` on the Borrowed case (plain UTF-8, the
    // common case) would copy the whole stream for nothing.
    let mut content = match crate::index::normalize_encoding(&raw) {
        std::borrow::Cow::Owned(converted) => converted,
        std::borrow::Cow::Borrowed(_) => raw,
    };

    // rg's binary searcher treats NUL as a line terminator too: `-c` counts
    // and `--json` line numbers split at NUL bytes, not only at `\n`
    // (characterized against rg 15.2.0). Mirror that for the matching half
    // by rewriting NUL to `\n` when the stream is binary. Line-printing
    // modes never see these lines (the notice replaces them below).
    let first_nul = content.iter().position(|&b| b == 0);
    if first_nul.is_some() {
        for b in content.iter_mut() {
            if *b == 0 {
                *b = b'\n';
            }
        }
    }

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
        // Mirror the indexed path's rule (scope/mod.rs): -l/-q only consume
        // paths, so skip the per-line content copy for them. -L reaches here
        // too and also never reads line content.
        verify_regex(
            &re,
            &label,
            &content,
            filter_args.files_with_matches || filter_args.files_without_match || filter_args.quiet,
        )
    };
    let mut matches = super::post_filter::apply_post_filters(matches, &filter_args, &[]);

    // rg binary policy (characterized against rg 15.2.0): with a NUL in the
    // stream, every line-printing mode replaces ALL of its output with a
    // single `binary file matches` notice (match position relative to the
    // NUL is irrelevant); -c/-l/-q/--json keep the normal output; a stream
    // with no match at all stays silent and exits 1. Under -v the notice
    // always wins (rg prints it even when the inverted output would be
    // empty). The notice reports the ORIGINAL first-NUL offset (the content
    // was rewritten to `\n` above for matching).
    let binary_notice = if first_nul.is_some()
        && !filter_args.count
        && !filter_args.count_matches
        && !filter_args.files_with_matches
        // -L is a listing mode like -l: no line output to replace.
        && !filter_args.files_without_match
        && !filter_args.quiet
        && !filter_args.json
        // rg prints the notice only when there is line output to replace:
        // matched lines normally, non-matching lines under -v. An empty
        // result (including an empty invert) stays silent and exits 1.
        && !matches.is_empty()
    {
        matches.clear();
        first_nul.map(|nul| nul as u64)
    } else {
        None
    };

    let mut files = HashMap::new();
    files.insert(
        label,
        crate::search::MatchedFile {
            normalized: content.into(),
            raw_len,
            first_nul: first_nul.map(|n| n as u64),
        },
    );
    Ok((matches, files, binary_notice))
}

#[cfg(test)]
#[path = "stdin_search_tests.rs"]
mod tests;

mod run;
pub(in crate::cli) use run::print_binary_notice_exit_code;
use run::run_stdin_filter;
