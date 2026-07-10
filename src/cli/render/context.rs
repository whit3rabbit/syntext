//! Context-aware output renderer: matches with surrounding lines.

use std::collections::BTreeSet;
use std::io;

use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use super::{
    compile_output_regex, group_matches_by_path, match_spans, read_matched_file,
    repo_canonical_root, write_formatted_line, ColorStyles,
};
use super::color::write_styled;
use crate::cli::search::SearchArgs;

/// Print matches with surrounding context lines to stdout.
///
/// Lines from context (not the match itself) use `-` as the separator; match lines use `:`.
/// Blocks separated by a gap in line numbers emit a `--` context separator.
pub(in crate::cli) fn render_with_context(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    render_with_context_to(config, matches, args, &mut out)
}

/// Write matches with surrounding context lines to any writer (used for testing).
///
/// Lines from context (not the match itself) use `-` as the separator; match lines use `:`.
/// Blocks separated by a gap in line numbers emit a `--` context separator.
pub(in crate::cli) fn render_with_context_to(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    out: &mut dyn std::io::Write,
) -> io::Result<()> {
    let by_file = group_matches_by_path(matches);

    let before = args.before_context;
    let after = args.after_context;
    let grouped_heading = args.heading && (by_file.len() > 1 || !args.no_filename);
    let suppress_path_prefix = args.heading;

    let canonical_root = repo_canonical_root(config);
    // Only needed to compute match spans for highlighting; uncompiled when color
    // is off so non-colored context output pays no regex cost.
    let re = args
        .color
        .then(|| compile_output_regex(args))
        .transpose()?;
    let styles = ColorStyles::default();
    let mut first_file = true;
    for (rel_path, match_lines) in &by_file {
        let Some(raw_content) = read_matched_file(config, &canonical_root, rel_path, args.quiet)
        else {
            continue;
        };
        let file_content = crate::index::normalize_encoding(&raw_content, config.verbose);
        // Keep the line-start byte offset alongside each line so -b/--byte-offset
        // can print it (json.rs keeps the same (line_start, line) pair).
        let mut file_lines: Vec<(usize, Vec<u8>)> = Vec::new();
        for_each_line(file_content.as_ref(), |_, line_start, line| {
            file_lines.push((line_start, line.to_vec()))
        });

        // Set of 0-based line indices that are direct matches.
        let match_set: BTreeSet<usize> = match_lines
            .iter()
            .map(|&n| (n as usize).saturating_sub(1))
            .collect();

        // Union of all context windows around each match.
        let mut to_print: BTreeSet<usize> = BTreeSet::new();
        for &mi in &match_set {
            let start = mi.saturating_sub(before);
            let end = (mi + after).min(file_lines.len().saturating_sub(1));
            for i in start..=end {
                to_print.insert(i);
            }
        }

        // Ripgrep uses blank lines between headed file groups, but context separator for flat.
        if !first_file {
            if grouped_heading {
                writeln!(out)?;
            } else {
                writeln!(out, "{}", args.context_separator)?;
            }
        }
        if grouped_heading && !args.no_filename {
            write_styled(out, args.color, styles.path, path_bytes(rel_path).as_ref())?;
            if args.null {
                out.write_all(b"\0")?;
            } else {
                out.write_all(b"\n")?;
            }
        }
        first_file = false;

        let mut prev: Option<usize> = None;
        for &idx in &to_print {
            // Gap separator between non-contiguous context blocks.
            if let Some(p) = prev {
                if idx > p + 1 {
                    writeln!(out, "{}", args.context_separator)?;
                }
            }

            let line_num = idx + 1;
            let (line_start, content) = file_lines
                .get(idx)
                .map(|(s, l)| (*s, l.as_slice()))
                .unwrap_or((0, &[][..]));
            let is_match = match_set.contains(&idx);
            let sep = if is_match { b':' } else { b'-' };

            if args.byte_offset {
                // Line-start offset prefix, matching render_flat_to/rg.
                write!(out, "{line_start}:")?;
            }
            let spans = if is_match {
                match_spans(re.as_ref(), content)
            } else {
                Vec::new()
            };
            write_formatted_line(
                out,
                super::FormatOpts {
                    no_path: suppress_path_prefix || args.no_filename,
                    no_num: args.no_line_number,
                    null: args.null,
                    color: args.color,
                },
                rel_path,
                line_num,
                sep,
                content,
                &spans,
            )?;

            prev = Some(idx);
        }
    }
    Ok(())
}
