//! Execution of a decided stdin search: read, match, and render the stream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::render;
use super::super::search::render_results;
use super::SearchArgs;
use crate::search::MatchedFile;

/// Entry point for a pure stdin search (`StdinDecision::UseStdin`): read the
/// stream to EOF, verify it, and render the result. Unlike
/// [`run_or_collect_stdin`](super::run_or_collect_stdin), there is no index
/// half to merge -- this is the whole search.
pub(super) fn run_stdin_filter(config: &crate::Config, args: &SearchArgs) -> i32 {
    let start = Instant::now();
    let (matches, files, binary_notice) = match super::collect_stdin(args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    render_stdin_half(
        config,
        args,
        matches,
        files,
        binary_notice,
        true,
        start.elapsed(),
    )
}

/// Render an already-collected stdin search. `single_input` selects
/// ripgrep's single-input naming rule (no filename prefix unless -H) for a
/// lone `-`; the all-dashes form (`st pat - -`) keeps the multi-input naming
/// the dash list implies, like rg.
pub(super) fn render_stdin_half(
    config: &crate::Config,
    args: &SearchArgs,
    matches: Vec<crate::SearchMatch>,
    files: HashMap<PathBuf, MatchedFile>,
    binary_notice: Option<u64>,
    single_input: bool,
    elapsed: Duration,
) -> i32 {
    let mut output_args = args.with_effective_output_defaults(config);
    if single_input && !output_args.with_filename {
        output_args.no_filename = true;
    }
    if let Some(offset) = binary_notice {
        return print_binary_notice_exit_code(offset, output_args.no_filename, output_args.vimgrep);
    }
    // Some(true): the stream is this search's only input, so -L lists it
    // first (and alone).
    render_results(
        config,
        None,
        matches,
        files,
        &output_args,
        elapsed,
        Some(true),
    )
}

/// Per-line invert for stdin streams. st's indexed `-v` is corpus-wide (list
/// non-matching files), which is meaningless for a single stream; a piped
/// `st -v` therefore inverts line-by-line, like `rg -v` in a pipe.
pub(super) fn invert_matches(
    re: &regex::bytes::Regex,
    path: &Path,
    content: &[u8],
) -> Vec<crate::SearchMatch> {
    let mut out = Vec::new();
    render::for_each_inverted_line(content, re, |line_num, line_start, display| {
        out.push(crate::SearchMatch {
            path: path.to_path_buf(),
            line_number: line_num,
            // rg -v prints the raw line, `\r` included.
            line_content: display.to_vec(),
            // No submatch exists on an inverted line; report the line's
            // own offset so -b/--byte-offset stays truthful.
            byte_offset: line_start as u64,
            submatch_start: 0,
            submatch_end: 0,
        });
    });
    out
}

/// rg's notice for binary input, honoring the mode's filename rules
/// (`--vimgrep` always prefixes; the flat default does only when filenames
/// are shown).
fn print_binary_notice(nul_offset: u64, no_filename: bool, vimgrep: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if !no_filename || vimgrep {
        // rg's notice format is `path: text` (note the space), unlike its
        // `path:line:content` match format.
        write!(out, "{}: ", super::STDIN_LABEL)?;
    }
    writeln!(
        out,
        "binary file matches (found \"\\0\" byte around offset {nul_offset})"
    )
}

/// [`print_binary_notice`], converting a write failure into the same
/// exit-code convention every other renderer uses (see `handle_output` in
/// `cli::search::output`): 0 for a broken pipe (the reader hung up, not a
/// real error), 2 with a stderr message for anything else. Both stdin entry
/// points that print this notice -- the pure-stream path here (in
/// `render_stdin_half`) and the mixed stdin+paths path in `search.rs` -- do
/// so outside of `render_results`'s own `io::Result` plumbing, so without
/// this wrapper a genuine write failure (e.g. a full disk) was silently
/// swallowed and always reported as a successful (exit 0) search.
pub(in crate::cli) fn print_binary_notice_exit_code(
    nul_offset: u64,
    no_filename: bool,
    vimgrep: bool,
) -> i32 {
    match print_binary_notice(nul_offset, no_filename, vimgrep) {
        Ok(()) => 0,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            eprintln!("st: {e}");
            2
        }
    }
}
