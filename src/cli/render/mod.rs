//! Output rendering: flat, heading, invert-match, context, and JSON formats.
// io::Error::new(ErrorKind::Other, ...) is used instead of io::Error::other()
// for Rust < 1.74 compatibility (Windows CI constraint).
#![allow(clippy::io_other_error)]

mod color;
mod context;
mod count;
mod flat;
mod invert;
mod json;
mod only_matching;

// Re-export extracted renderers so callers can still use `super::render::*`.
pub(super) use context::render_with_context;
#[cfg(test)]
pub(super) use context::render_with_context_to;
pub(super) use count::render_count_matches;
pub(super) use invert::render_invert_match;
pub(super) use json::render_json;
pub(super) use only_matching::render_only_matching;
// Color decision + fixed styles, resolved in `cli/mod.rs` and consumed by the
// renderers and `write_formatted_line` below.
pub(in crate::cli) use color::{resolve_color, ColorStyles, ColorWhen};

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::path_util::path_bytes;
use crate::Config;

use super::search::SearchArgs;
use crate::search::REGEX_SIZE_LIMIT;

pub(in crate::cli) fn group_matches_by_path(
    matches: &[crate::SearchMatch],
) -> std::collections::BTreeMap<PathBuf, Vec<u32>> {
    let mut by_file = std::collections::BTreeMap::new();
    for m in matches {
        by_file
            .entry(m.path.clone())
            .or_insert_with(Vec::new)
            .push(m.line_number);
    }
    by_file
}

#[derive(Clone, Copy)]
pub(in crate::cli) struct FormatOpts {
    pub no_path: bool,
    pub no_num: bool,
    pub null: bool,
    /// Emit ANSI color for path/line-number/match text. When false, output is
    /// byte-identical to the uncolored path (spans are ignored).
    pub color: bool,
}

/// Sorted, non-overlapping match byte spans into `content`, for highlighting.
/// Returns an empty vec when `re` is `None` (no regex was compiled).
pub(in crate::cli) fn match_spans(
    re: Option<&regex::bytes::Regex>,
    content: &[u8],
) -> Vec<(usize, usize)> {
    match re {
        Some(r) => r
            .find_iter(content)
            .map(|m| (m.start(), m.end()))
            .filter(|(s, e)| s < e)
            .collect(),
        None => Vec::new(),
    }
}

pub(in crate::cli) fn write_formatted_line(
    out: &mut dyn Write,
    opts: FormatOpts,
    path: &Path,
    line_num: usize,
    sep: u8,
    content: &[u8],
    spans: &[(usize, usize)],
) -> io::Result<()> {
    let styles = ColorStyles::default();
    match (opts.no_path, opts.no_num) {
        (true, true) => color::write_highlighted(out, opts.color, styles, content, spans)?,
        (true, false) => {
            color::write_styled_num(out, opts.color, styles.line, line_num)?;
            write!(out, "{}", sep as char)?;
            color::write_highlighted(out, opts.color, styles, content, spans)?;
        }
        (false, true) => {
            color::write_styled(out, opts.color, styles.path, &path_bytes(path))?;
            let path_sep = if opts.null { b'\0' } else { sep };
            out.write_all(&[path_sep])?;
            color::write_highlighted(out, opts.color, styles, content, spans)?;
        }
        (false, false) => {
            color::write_styled(out, opts.color, styles.path, &path_bytes(path))?;
            let path_sep = if opts.null { b'\0' } else { sep };
            out.write_all(&[path_sep])?;
            color::write_styled_num(out, opts.color, styles.line, line_num)?;
            write!(out, "{}", sep as char)?;
            color::write_highlighted(out, opts.color, styles, content, spans)?;
        }
    }
    out.write_all(b"\n")
}

pub(in crate::cli) fn json_data(bytes: &[u8]) -> serde_json::Value {
    if let Ok(text) = std::str::from_utf8(bytes) {
        serde_json::json!({ "text": text })
    } else {
        serde_json::json!({ "bytes": crate::base64::encode(bytes) })
    }
}

pub(in crate::cli) fn json_stats(
    elapsed: Duration,
    searches: usize,
    searches_with_match: usize,
    bytes_searched: usize,
    bytes_printed: usize,
    matched_lines: usize,
    matches: usize,
) -> serde_json::Value {
    serde_json::json!({
        "elapsed": json_elapsed(elapsed),
        "searches": searches,
        "searches_with_match": searches_with_match,
        "bytes_searched": bytes_searched,
        "bytes_printed": bytes_printed,
        "matched_lines": matched_lines,
        "matches": matches
    })
}

pub(in crate::cli) fn json_elapsed(elapsed: Duration) -> serde_json::Value {
    let human = if elapsed.is_zero() {
        "0s".to_string()
    } else if elapsed.as_secs() == 0 {
        format!("{:.6}s", elapsed.as_secs_f64())
    } else {
        format!("{:.3}s", elapsed.as_secs_f64())
    };
    serde_json::json!({
        "secs": elapsed.as_secs(),
        "nanos": elapsed.subsec_nanos(),
        "human": human
    })
}

pub(in crate::cli) fn json_submatches(
    re: &regex::bytes::Regex,
    line: &[u8],
) -> Vec<serde_json::Value> {
    re.find_iter(line)
        .map(|matched| {
            serde_json::json!({
                "match": json_data(&line[matched.start()..matched.end()]),
                "start": matched.start(),
                "end": matched.end()
            })
        })
        .collect()
}

pub(in crate::cli) fn json_line_message(
    message_type: &str,
    path: &Path,
    line_number: usize,
    absolute_offset: usize,
    line: &[u8],
    submatches: Vec<serde_json::Value>,
) -> String {
    let mut line_with_newline = line.to_vec();
    line_with_newline.push(b'\n');
    serde_json::json!({
        "type": message_type,
        "data": {
            "path": json_data(path_bytes(path).as_ref()),
            "lines": json_data(&line_with_newline),
            "line_number": line_number,
            "absolute_offset": absolute_offset,
            "submatches": submatches
        }
    })
    .to_string()
}

pub(in crate::cli) fn write_json_line(out: &mut dyn Write, line: &str) -> io::Result<usize> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(line.len() + 1)
}

/// Canonicalized repo root, computed once per render call (not per file) to
/// avoid a `realpath` syscall on every matched file. Falls back to the
/// non-canonical root if resolution fails; per-file canonicalize + O_NOFOLLOW
/// + fd/stat verification still guard each read.
pub(in crate::cli) fn repo_canonical_root(config: &Config) -> PathBuf {
    config
        .repo_root
        .canonicalize()
        .unwrap_or_else(|_| config.repo_root.clone())
}

/// Read a matched file's bytes for re-rendering. Returns `None` when the file
/// is unreadable: silently for `NotFound` (file deleted between match and
/// render, normal in agent workflows), with a stderr warning otherwise so a
/// file that grew past `max_file_size` or failed verification is not silently
/// dropped from `--count`/`--json`/context/only-matching/invert output.
pub(in crate::cli) fn read_matched_file(
    config: &Config,
    canonical_root: &Path,
    rel_path: &Path,
    quiet: bool,
) -> Option<Vec<u8>> {
    match read_repo_file_bytes(config, canonical_root, rel_path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            if !quiet {
                eprintln!(
                    "st: matched file unreadable, omitted from output: {}: {e}",
                    rel_path.display()
                );
            }
            None
        }
    }
}

pub(in crate::cli) fn read_repo_file_bytes(
    config: &Config,
    canonical_root: &Path,
    rel_path: &Path,
) -> io::Result<Vec<u8>> {
    let abs_path = config.repo_root.join(rel_path);

    // Verify containment under the repo root, matching the hardened open path
    // in search/resolver.rs::resolve_doc. Without this, a symlink swap between
    // index time and render time could redirect the second read outside the
    // repo (information disclosure). `canonical_root` is computed once per
    // render call by the caller. A NotFound (file vanished between match and
    // render) surfaces as a normal io::Error to the caller.
    let canonical = abs_path.canonicalize()?;
    if !canonical.starts_with(canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resolved path escapes repo root",
        ));
    }

    #[cfg(any(unix, windows))]
    let pre_open_meta = std::fs::metadata(&canonical)?;
    let file = crate::index::open_readonly_nofollow(&canonical)?;
    #[cfg(any(unix, windows))]
    if !crate::index::verify_fd_matches_stat(&file, &pre_open_meta) {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "path changed during verification",
        ));
    }

    // Bound the read at config.max_file_size (+1 sentinel) so a file that grew
    // to gigabytes between index time and render time cannot trigger an
    // unbounded read_to_end allocation. saturating_add guards against
    // max_file_size == u64::MAX (would otherwise wrap to 0 and read nothing).
    // ErrorKind::Other (not FileTooLarge, which needs Rust 1.83) keeps the
    // documented <1.74 MSRV; callers distinguish "grew" from "vanished" via
    // read_matched_file rather than by error kind.
    let mut reader = file.take(config.max_file_size.saturating_add(1));
    let mut raw_content = Vec::new();
    reader.read_to_end(&mut raw_content)?;
    if raw_content.len() as u64 > config.max_file_size {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "file grew beyond max_file_size since index time",
        ));
    }
    Ok(raw_content)
}

/// Build the `(routing_pattern, verify_pattern)` pair for a search.
///
/// - `routing_pattern`: the raw pattern used for gram extraction.
/// - `verify_pattern`: when `-w` or `-x` is set, the boundary-wrapped
///   pattern for verification; otherwise `None`.
///
/// Separating these prevents the wrapped regex from being routed through
/// the HIR walker, which would reject boundary-hugging grams and force
/// every `-w`/`-x` query into a full scan.
pub(in crate::cli) fn build_effective_pattern(args: &SearchArgs) -> (String, Option<String>) {
    let pat = if args.fixed_strings {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };
    if args.line_regexp {
        let wrapped = format!("^(?:{pat})$");
        (pat, Some(wrapped))
    } else if args.word_regexp {
        // Group pat so a multi-`-e` alternation like `(?:a)|(?:b)` anchors on
        // both sides of every alternative. Without the inner group, `\b` would
        // bind only the first/last alternative (`\b(?:a)|(?:b)\b`).
        let wrapped = format!(r"\b(?:{pat})\b");
        (pat, Some(wrapped))
    } else {
        (pat, None)
    }
}

pub(in crate::cli) fn compile_output_regex(args: &SearchArgs) -> io::Result<regex::bytes::Regex> {
    let (routing, verify) = build_effective_pattern(args);
    let pattern = verify.as_deref().unwrap_or(&routing);
    regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(args.ignore_case)
        .multi_line(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
}

#[cfg(test)]
pub(super) fn format_match_json(m: &crate::SearchMatch) -> String {
    let submatch = serde_json::json!({
        "match": json_data(&m.line_content[m.submatch_start..m.submatch_end]),
        "start": m.submatch_start,
        "end": m.submatch_end
    });
    let line_start = m.byte_offset.saturating_sub(m.submatch_start as u64) as usize;
    json_line_message(
        "match",
        &m.path,
        m.line_number as usize,
        line_start,
        &m.line_content,
        vec![submatch],
    )
}

// Flat, heading, and vimgrep renderers live in `flat.rs` to keep this file
// under the 400-line quality gate; re-exported so callers keep using
// `render::render_flat` etc.
pub(in crate::cli) use flat::{render_flat, render_heading, render_vimgrep};
#[cfg(test)]
pub(in crate::cli) use flat::{render_flat_to, render_heading_to, render_vimgrep_to};
