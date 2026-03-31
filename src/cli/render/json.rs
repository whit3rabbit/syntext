//! JSON (NDJSON) output renderer: rg-compatible begin/match/context/end/summary.

use std::io::{self, Write};
use std::time::Instant;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use crate::cli::search::{collect_scoped_paths, SearchArgs};
use super::{
    compile_output_regex, group_matches_by_path, json_data, json_elapsed, json_line_message,
    json_stats, json_submatches, read_repo_file_bytes, write_json_line,
};

/// Emit rg-compatible NDJSON for all matches: begin/match.../end per file + summary.
pub(in crate::cli) fn render_json(
    index: &Index,
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    let total_start = Instant::now();
    let re = compile_output_regex(args)?;
    let by_file = group_matches_by_path(matches);
    let before = args.before_context;
    let after = args.after_context;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut total_bytes_searched = 0usize;
    let mut total_bytes_printed = 0usize;
    let mut total_matched_lines = 0usize;
    let mut total_matches = 0usize;
    let scoped_paths = collect_scoped_paths(index, config, args);
    let total_searches = scoped_paths.len();
    let searches_with_match = by_file.len();

    for (path, match_lines) in &by_file {
        let file_start = Instant::now();
        let Ok(raw_content) = read_repo_file_bytes(config, path) else {
            continue;
        };
        total_bytes_searched += raw_content.len();
        let file_content = crate::index::normalize_encoding(&raw_content, config.verbose);
        let mut file_lines: Vec<(usize, usize, Vec<u8>)> = Vec::new();
        for_each_line(file_content.as_ref(), |line_num, line_start, line| {
            file_lines.push((line_num as usize, line_start, line.to_vec()))
        });

        let match_set: std::collections::BTreeSet<usize> = match_lines
            .iter()
            .map(|&n| (n as usize).saturating_sub(1))
            .collect();
        let mut to_print: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        for &mi in &match_set {
            let start = mi.saturating_sub(before);
            let end = (mi + after).min(file_lines.len().saturating_sub(1));
            for idx in start..=end {
                to_print.insert(idx);
            }
        }

        let mut file_matched_lines = 0usize;
        let mut file_total_matches = 0usize;

        // begin
        let mut file_bytes_printed = 0usize;
        let begin = serde_json::json!({"type":"begin","data":{"path": json_data(path_bytes(path).as_ref())}})
            .to_string();
        file_bytes_printed += write_json_line(&mut out, &begin)?;

        for idx in to_print {
            let Some((line_number, line_start, line)) = file_lines.get(idx) else {
                continue;
            };
            if match_set.contains(&idx) {
                let submatches = json_submatches(&re, line);
                file_matched_lines += 1;
                file_total_matches += submatches.len();
                let message =
                    json_line_message("match", path, *line_number, *line_start, line, submatches);
                file_bytes_printed += write_json_line(&mut out, &message)?;
            } else {
                let message =
                    json_line_message("context", path, *line_number, *line_start, line, Vec::new());
                file_bytes_printed += write_json_line(&mut out, &message)?;
            }
        }

        total_bytes_printed += file_bytes_printed;
        total_matched_lines += file_matched_lines;
        total_matches += file_total_matches;

        // end
        let end = serde_json::json!({
            "type": "end",
            "data": {
                "path": json_data(path_bytes(path).as_ref()),
                "binary_offset": null,
                "stats": json_stats(
                    file_start.elapsed(),
                    1,
                    1,
                    raw_content.len(),
                    file_bytes_printed,
                    file_matched_lines,
                    file_total_matches
                )
            }
        })
        .to_string();
        write_json_line(&mut out, &end)?;
    }

    for path in scoped_paths {
        if by_file.contains_key(&path) {
            continue;
        }
        if let Ok(raw_content) = read_repo_file_bytes(config, &path) {
            total_bytes_searched += raw_content.len();
        }
    }

    // summary
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "type": "summary",
                "data": {
                    "elapsed_total": json_elapsed(total_start.elapsed()),
                    "stats": json_stats(
                        total_start.elapsed(),
                        total_searches,
                        searches_with_match,
                        total_bytes_searched,
                        total_bytes_printed,
                        total_matched_lines,
                        total_matches
                    )
            }
        })
    )?;
    Ok(())
}
