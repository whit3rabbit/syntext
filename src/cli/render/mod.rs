//! Output rendering: flat, heading, invert-match, context, and JSON formats.
// io::Error::new(ErrorKind::Other, ...) is used instead of io::Error::other()
// for Rust < 1.74 compatibility (Windows CI constraint).
#![allow(clippy::io_other_error)]

mod context;
mod count;
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

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::path_util::path_bytes;
use crate::Config;

use super::search::{build_effective_pattern, SearchArgs};
use crate::search::REGEX_SIZE_LIMIT;

pub(in crate::cli) fn write_formatted_line(
    out: &mut dyn Write,
    no_path: bool,
    no_num: bool,
    path: &Path,
    line_num: usize,
    sep: u8,
    content: &[u8],
) -> io::Result<()> {
    match (no_path, no_num) {
        (true, true) => out.write_all(content)?,
        (true, false) => {
            write!(out, "{line_num}{}", sep as char)?;
            out.write_all(content)?;
        }
        (false, true) => {
            out.write_all(&path_bytes(path))?;
            out.write_all(&[sep])?;
            out.write_all(content)?;
        }
        (false, false) => {
            out.write_all(&path_bytes(path))?;
            write!(out, "{}{line_num}{}", sep as char, sep as char)?;
            out.write_all(content)?;
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

pub(in crate::cli) fn read_repo_file_bytes(
    config: &Config,
    rel_path: &Path,
) -> io::Result<Vec<u8>> {
    let abs_path = config.repo_root.join(rel_path);

    #[cfg(unix)]
    let pre_open_meta = std::fs::metadata(&abs_path)?;
    let mut file = crate::index::open_readonly_nofollow(&abs_path)?;
    #[cfg(unix)]
    if !crate::index::verify_fd_matches_stat(&file, &pre_open_meta) {
        return Err(io::Error::new(io::ErrorKind::Other, "path changed during verification"));
    }

    let mut raw_content = Vec::new();
    file.read_to_end(&mut raw_content)?;
    Ok(raw_content)
}

pub(in crate::cli) fn compile_output_regex(
    args: &SearchArgs,
) -> io::Result<regex::bytes::Regex> {
    regex::bytes::RegexBuilder::new(&build_effective_pattern(args))
        .case_insensitive(args.ignore_case)
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

pub(super) fn render_flat(matches: &[crate::SearchMatch], args: &SearchArgs) -> io::Result<()> {
    render_flat_to(matches, args, &mut io::stdout().lock())
}

pub(in crate::cli) fn render_flat_to(
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    out: &mut dyn Write,
) -> io::Result<()> {
    let re = if args.replace.is_some() || args.column {
        Some(compile_output_regex(args)?)
    } else {
        None
    };
    for m in matches {
        let raw = apply_replace(re.as_ref(), args.replace.as_deref(), &m.line_content);
        let Some(line) = apply_output_modifiers(&raw, args) else { continue };
        if args.byte_offset {
            write!(out, "{}:", m.byte_offset)?;
        }
        if args.column && !args.no_filename && !args.no_line_number {
            out.write_all(&path_bytes(&m.path))?;
            write!(out, ":{}:{}:", m.line_number, m.submatch_start + 1)?;
            out.write_all(&line)?;
            out.write_all(b"\n")?;
        } else {
            write_formatted_line(
                out,
                args.no_filename,
                args.no_line_number,
                &m.path,
                m.line_number as usize,
                b':',
                &line,
            )?;
        }
    }
    Ok(())
}

pub(super) fn render_heading(matches: &[crate::SearchMatch], args: &SearchArgs) -> io::Result<()> {
    render_heading_to(matches, args, &mut io::stdout().lock())
}

pub(in crate::cli) fn render_heading_to(
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    out: &mut dyn Write,
) -> io::Result<()> {
    let re = if args.replace.is_some() {
        Some(compile_output_regex(args)?)
    } else {
        None
    };
    let mut current_path: Option<PathBuf> = None;
    for m in matches {
        if current_path.as_ref() != Some(&m.path) {
            if current_path.is_some() {
                writeln!(out)?;
            }
            out.write_all(path_bytes(&m.path).as_ref())?;
            out.write_all(b"\n")?;
            current_path = Some(m.path.clone());
        }
        let raw = apply_replace(re.as_ref(), args.replace.as_deref(), &m.line_content);
        let Some(line) = apply_output_modifiers(&raw, args) else { continue };
        if args.byte_offset {
            write!(out, "{}:", m.byte_offset)?;
        }
        if args.no_line_number {
            out.write_all(&line)?;
            out.write_all(b"\n")?;
        } else if args.column {
            write!(out, "{}:{}:", m.line_number, m.submatch_start + 1)?;
            out.write_all(&line)?;
            out.write_all(b"\n")?;
        } else {
            write!(out, "{}:", m.line_number)?;
            out.write_all(&line)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

/// Emit one output line per match in path:line:column:content format.
pub(super) fn render_vimgrep(
    _config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    render_vimgrep_to(matches, args, &mut io::stdout().lock())
}

pub(in crate::cli) fn render_vimgrep_to(
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    out: &mut dyn Write,
) -> io::Result<()> {
    let re = compile_output_regex(args)?;
    for m in matches {
        let raw = apply_replace(
            args.replace.as_ref().map(|_| &re),
            args.replace.as_deref(),
            &m.line_content,
        );
        let Some(line) = apply_output_modifiers(&raw, args) else { continue };
        for hit in re.find_iter(&m.line_content) {
            if hit.start() == hit.end() {
                continue;
            }
            let col = hit.start() + 1; // 1-based
            if args.byte_offset {
                write!(out, "{}:", m.byte_offset)?;
            }
            out.write_all(&path_bytes(&m.path))?;
            write!(out, ":{}:{col}:", m.line_number)?;
            out.write_all(&line)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

/// Apply --replace to line content, returning the original slice if no replacement is needed.
fn apply_replace<'a>(
    re: Option<&regex::bytes::Regex>,
    replacement: Option<&str>,
    line: &'a [u8],
) -> std::borrow::Cow<'a, [u8]> {
    match (re, replacement) {
        (Some(re), Some(repl)) => {
            std::borrow::Cow::Owned(re.replace_all(line, repl.as_bytes()).into_owned())
        }
        _ => std::borrow::Cow::Borrowed(line),
    }
}

/// Apply --trim and --max-columns to a line. Returns None if the line should be skipped.
fn apply_output_modifiers<'a>(line: &'a [u8], args: &SearchArgs) -> Option<std::borrow::Cow<'a, [u8]>> {
    let trimmed: &[u8] = if args.trim {
        let start = line.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(line.len());
        &line[start..]
    } else {
        line
    };
    if let Some(max) = args.max_columns {
        if trimmed.len() > max {
            return None;
        }
    }
    Some(std::borrow::Cow::Borrowed(trimmed))
}
