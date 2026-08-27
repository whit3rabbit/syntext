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
pub(in crate::cli) use invert::for_each_inverted_line;
pub(super) use invert::render_invert_match;
pub(super) use json::render_json;
pub(super) use only_matching::render_only_matching;
// Color decision + fixed styles, resolved in `cli/mod.rs` and consumed by the
// renderers and `write_formatted_line` below.
pub(in crate::cli) use color::{resolve_color, ColorStyles, ColorWhen};

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

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
    /// rg field order puts the byte offset LAST among the prefix fields,
    /// immediately before the content (`[path:][line:][col:]byte:content`),
    /// and it keeps the line separator ('-' for context lines) after it.
    pub byte_offset: Option<u64>,
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
    // rg's byte-offset field comes last among the prefix fields, directly
    // before the content, keeping the line separator after it.
    let byte_prefix = |out: &mut dyn Write| -> io::Result<()> {
        if let Some(byte) = opts.byte_offset {
            write!(out, "{byte}{}", sep as char)?;
        }
        Ok(())
    };
    match (opts.no_path, opts.no_num) {
        (true, true) => {
            byte_prefix(out)?;
            color::write_highlighted(out, opts.color, styles, content, spans)?
        }
        (true, false) => {
            color::write_styled_num(out, opts.color, styles.line, line_num)?;
            write!(out, "{}", sep as char)?;
            byte_prefix(out)?;
            color::write_highlighted(out, opts.color, styles, content, spans)?;
        }
        (false, true) => {
            color::write_styled(out, opts.color, styles.path, &path_bytes(path))?;
            let path_sep = if opts.null { b'\0' } else { sep };
            out.write_all(&[path_sep])?;
            byte_prefix(out)?;
            color::write_highlighted(out, opts.color, styles, content, spans)?;
        }
        (false, false) => {
            color::write_styled(out, opts.color, styles.path, &path_bytes(path))?;
            let path_sep = if opts.null { b'\0' } else { sep };
            out.write_all(&[path_sep])?;
            color::write_styled_num(out, opts.color, styles.line, line_num)?;
            write!(out, "{}", sep as char)?;
            byte_prefix(out)?;
            color::write_highlighted(out, opts.color, styles, content, spans)?;
        }
    }
    out.write_all(b"\n")
}

mod json_helpers;
pub(in crate::cli) use json_helpers::{
    json_data, json_elapsed, json_line_message, json_stats, json_submatches, rendered_line,
    write_json_line,
};

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

/// Encoding-normalized bytes of a matched file for rendering, preferring the
/// content verified during search (`files`). Reusing those bytes keeps output
/// consistent with the match snapshot when a file churns between search and
/// render, and skips a redundant disk read + re-normalize. Falls back to the
/// hardened disk read for paths absent from the map (symbol/refs searches carry
/// no content map; also a defensive fallback). Returns `None` when the file is
/// both absent from the map and unreadable.
pub(in crate::cli) fn matched_file_bytes<'a>(
    files: &'a std::collections::HashMap<std::path::PathBuf, crate::search::MatchedFile>,
    config: &Config,
    canonical_root: &Path,
    rel_path: &Path,
    quiet: bool,
) -> Option<std::borrow::Cow<'a, [u8]>> {
    if let Some(mf) = files.get(rel_path) {
        return Some(std::borrow::Cow::Borrowed(mf.normalized.as_ref()));
    }
    read_matched_file(config, canonical_root, rel_path, quiet)
        .map(|raw| std::borrow::Cow::Owned(crate::index::normalize_encoding(&raw).into_owned()))
}

pub(in crate::cli) fn read_repo_file_bytes(
    config: &Config,
    canonical_root: &Path,
    rel_path: &Path,
) -> io::Result<Vec<u8>> {
    // Open guaranteed-beneath the repo root: one openat2(RESOLVE_BENEATH) on
    // Linux (atomic containment; closes the intermediate-component TOCTOU window
    // a canonicalize-then-open sequence leaves open), else the portable
    // canonicalize + stat + O_NOFOLLOW + fd-verify path. Without containment a
    // symlink swap between index time and render time could redirect this second
    // read outside the repo (information disclosure). On open failure, preserve
    // the NotFound-vs-other distinction `read_matched_file` uses to choose
    // silent-skip vs warn, probing existence only on the rare failure path.
    let file = match crate::index::io_util::open_beneath_fresh(canonical_root, rel_path) {
        Some(f) => f,
        None => {
            let abs_path = config.repo_root.join(rel_path);
            return Err(if !abs_path.exists() {
                io::Error::new(io::ErrorKind::NotFound, "matched file no longer exists")
            } else {
                io::Error::other("matched file could not be securely opened")
            });
        }
    };

    // Bound the read at config.max_file_size (+1 sentinel) so a file that grew
    // to gigabytes between index time and render time cannot trigger an
    // unbounded read_to_end allocation. saturating_add guards against
    // max_file_size == u64::MAX (would otherwise wrap to 0 and read nothing).
    // The "grew" error is `other`, so read_matched_file warns rather than
    // silently dropping it (only NotFound is silent).
    let mut reader = file.take(config.max_file_size.saturating_add(1));
    let mut raw_content = Vec::new();
    reader.read_to_end(&mut raw_content)?;
    if raw_content.len() as u64 > config.max_file_size {
        return Err(io::Error::other(
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
mod pattern;
pub(in crate::cli) use pattern::build_effective_pattern;

pub(in crate::cli) fn compile_output_regex(args: &SearchArgs) -> io::Result<regex::bytes::Regex> {
    let (routing, verify) = build_effective_pattern(args);
    let pattern = verify.as_deref().unwrap_or(&routing);
    regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(args.ignore_case)
        .multi_line(true)
        // Match the verifier's CRLF mode (see search/mod.rs) so submatch
        // extraction agrees with the match decision: a `-x` match on a final
        // line "parse\r" must yield the "parse" submatch, not an empty one.
        .crlf(true)
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
