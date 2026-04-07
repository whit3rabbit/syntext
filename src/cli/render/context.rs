//! Context-aware output renderer: matches with surrounding lines.

use std::collections::BTreeSet;
use std::io;

use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use super::{group_matches_by_path, read_repo_file_bytes, write_formatted_line};
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

    let mut first_file = true;
    for (rel_path, match_lines) in &by_file {
        let raw_content = match read_repo_file_bytes(config, rel_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let file_content = crate::index::normalize_encoding(&raw_content, config.verbose);
        let mut file_lines: Vec<Vec<u8>> = Vec::new();
        for_each_line(file_content.as_ref(), |_, _, line| {
            file_lines.push(line.to_vec())
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
            out.write_all(path_bytes(rel_path).as_ref())?;
            out.write_all(b"\n")?;
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
            let content = file_lines.get(idx).map(Vec::as_slice).unwrap_or_default();
            let is_match = match_set.contains(&idx);
            let sep = if is_match { b':' } else { b'-' };

            write_formatted_line(
                out,
                suppress_path_prefix || args.no_filename,
                args.no_line_number,
                rel_path,
                line_num,
                sep,
                content,
            )?;

            prev = Some(idx);
        }
    }
    Ok(())
}
