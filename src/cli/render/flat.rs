//! Flat, heading, and vimgrep line renderers.
//!
//! Split from `render/mod.rs` to keep it under the 400-line quality gate.
//! These share the JSON/formatting helpers that remain in `mod.rs` (referenced
//! via `super::`).

use std::io::{self, Write};
use std::path::PathBuf;

use crate::path_util::path_bytes;
use crate::Config;

use super::color::{write_highlighted, write_styled, write_styled_num};
use super::{compile_output_regex, match_spans, write_formatted_line, ColorStyles, SearchArgs};

pub(in crate::cli) fn render_flat(
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    render_flat_to(matches, args, &mut io::stdout().lock())
}

pub(in crate::cli) fn render_flat_to(
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    out: &mut dyn Write,
) -> io::Result<()> {
    let re = if args.replace.is_some() || args.column || args.color {
        Some(compile_output_regex(args)?)
    } else {
        None
    };
    let styles = ColorStyles::default();
    for m in matches {
        let raw = apply_replace(re.as_ref(), args.replace.as_deref(), &m.line_content);
        let line = apply_output_modifiers(&raw, &m.line_content, re.as_ref(), args);
        if args.byte_offset {
            // rg prints the 0-based byte offset of the line start, not the
            // match. submatch_start is the match column within the line.
            let line_start = m.byte_offset.saturating_sub(m.submatch_start as u64);
            write!(out, "{}:", line_start)?;
        }
        if args.column {
            let spans = match_spans(re.as_ref(), &line.content);
            if !args.no_filename {
                write_styled(out, args.color, styles.path, &path_bytes(&m.path))?;
                let path_sep = if args.null { b'\0' } else { b':' };
                out.write_all(&[path_sep])?;
            }
            if !args.no_line_number {
                write_styled_num(out, args.color, styles.line, m.line_number as usize)?;
                write!(out, ":")?;
            }
            let col = (m.submatch_start + 1)
                .saturating_sub(line.trimmed_bytes)
                .max(1);
            write!(out, "{col}:")?;
            write_highlighted(out, args.color, styles, &line.content, &spans)?;
            out.write_all(b"\n")?;
        } else {
            let spans = match_spans(re.as_ref(), &line.content);
            write_formatted_line(
                out,
                super::FormatOpts {
                    no_path: args.no_filename,
                    no_num: args.no_line_number,
                    null: args.null,
                    color: args.color,
                },
                &m.path,
                m.line_number as usize,
                b':',
                &line.content,
                &spans,
            )?;
        }
    }
    Ok(())
}

pub(in crate::cli) fn render_heading(
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    render_heading_to(matches, args, &mut io::stdout().lock())
}

pub(in crate::cli) fn render_heading_to(
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    out: &mut dyn Write,
) -> io::Result<()> {
    // --column also needs the regex to count matches for the long-line
    // placeholder ([Omitted long line with N matches]); include it so the count
    // is exact instead of falling back to 1.
    let re = if args.replace.is_some() || args.color || args.column {
        Some(compile_output_regex(args)?)
    } else {
        None
    };
    let styles = ColorStyles::default();
    let mut current_path: Option<PathBuf> = None;
    for m in matches {
        if current_path.as_ref() != Some(&m.path) {
            if current_path.is_some() {
                writeln!(out)?;
            }
            write_styled(out, args.color, styles.path, path_bytes(&m.path).as_ref())?;
            if args.null {
                out.write_all(b"\0")?;
            } else {
                out.write_all(b"\n")?;
            }
            current_path = Some(m.path.clone());
        }
        let raw = apply_replace(re.as_ref(), args.replace.as_deref(), &m.line_content);
        let line = apply_output_modifiers(&raw, &m.line_content, re.as_ref(), args);
        let spans = match_spans(re.as_ref(), &line.content);
        if args.byte_offset {
            // rg prints the 0-based byte offset of the line start, not the
            // match. submatch_start is the match column within the line.
            let line_start = m.byte_offset.saturating_sub(m.submatch_start as u64);
            write!(out, "{}:", line_start)?;
        }
        if args.no_line_number {
            write_highlighted(out, args.color, styles, &line.content, &spans)?;
            out.write_all(b"\n")?;
        } else if args.column {
            let col = (m.submatch_start + 1)
                .saturating_sub(line.trimmed_bytes)
                .max(1);
            write_styled_num(out, args.color, styles.line, m.line_number as usize)?;
            write!(out, ":{col}:")?;
            write_highlighted(out, args.color, styles, &line.content, &spans)?;
            out.write_all(b"\n")?;
        } else {
            write_styled_num(out, args.color, styles.line, m.line_number as usize)?;
            write!(out, ":")?;
            write_highlighted(out, args.color, styles, &line.content, &spans)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

/// Emit one output line per match in path:line:column:content format.
pub(in crate::cli) fn render_vimgrep(
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
    let styles = ColorStyles::default();
    for m in matches {
        let raw = apply_replace(
            args.replace.as_ref().map(|_| &re),
            args.replace.as_deref(),
            &m.line_content,
        );
        let line = apply_output_modifiers(&raw, &m.line_content, Some(&re), args);
        let spans = match_spans(Some(&re), &line.content);
        for hit in re.find_iter(&m.line_content) {
            if hit.start() == hit.end() {
                continue;
            }
            // Column is the match position in the ORIGINAL line, matching rg:
            // --replace is a display transform, so with -r the printed content is
            // replaced but the column still points at the pre-replacement match.
            let col = hit.start() + 1; // 1-based
            let col = col.saturating_sub(line.trimmed_bytes).max(1);
            if args.byte_offset {
                // Line-start offset, matching render_flat_to/render_heading_to
                // and rg (byte-offset without -o is the line start, not the
                // match). Constant across hits on the same line.
                let line_start = m.byte_offset.saturating_sub(m.submatch_start as u64);
                write!(out, "{}:", line_start)?;
            }
            write_styled(out, args.color, styles.path, &path_bytes(&m.path))?;
            let path_sep = if args.null { b'\0' } else { b':' };
            out.write_all(&[path_sep])?;
            write_styled_num(out, args.color, styles.line, m.line_number as usize)?;
            write!(out, ":{col}:")?;
            write_highlighted(out, args.color, styles, &line.content, &spans)?;
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

struct OutputLine<'a> {
    content: std::borrow::Cow<'a, [u8]>,
    trimmed_bytes: usize,
    #[allow(dead_code)]
    is_omitted: bool,
}

fn get_omitted_placeholder(
    line_content: &[u8],
    count_re: Option<&regex::bytes::Regex>,
    args: &SearchArgs,
) -> Vec<u8> {
    if args.column || args.vimgrep {
        // Reuse the caller's already-compiled output regex; recompiling here
        // would rebuild the regex for every long line in this per-line hot path.
        // count_re is None only when no output regex was needed, in which case
        // fall back to 1 (matching the prior on-compile-failure behavior).
        let count = count_re.map_or(1, |re| re.find_iter(line_content).count());
        format!("[Omitted long line with {count} matches]").into_bytes()
    } else {
        b"[Omitted long matching line]".to_vec()
    }
}

/// Apply --trim and --max-columns to a line.
fn apply_output_modifiers<'a>(
    line: &'a [u8],
    line_content_for_placeholder: &[u8],
    count_re: Option<&regex::bytes::Regex>,
    args: &SearchArgs,
) -> OutputLine<'a> {
    let mut trimmed_bytes = 0;
    let trimmed: &[u8] = if args.trim {
        let start = line
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(line.len());
        trimmed_bytes = start;
        &line[start..]
    } else {
        line
    };
    if let Some(max) = args.max_columns {
        if trimmed.len() > max {
            let placeholder = get_omitted_placeholder(line_content_for_placeholder, count_re, args);
            return OutputLine {
                content: std::borrow::Cow::Owned(placeholder),
                trimmed_bytes: 0,
                is_omitted: true,
            };
        }
    }
    OutputLine {
        content: std::borrow::Cow::Borrowed(trimmed),
        trimmed_bytes,
        is_omitted: false,
    }
}
