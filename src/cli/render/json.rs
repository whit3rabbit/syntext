//! JSON (NDJSON) output renderer: rg-compatible begin/match/context/end/summary.

use std::io::{self, Write};
use std::time::Instant;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use super::{
    compile_output_regex, group_matches_by_path, json_data, json_elapsed, json_line_message,
    json_stats, json_submatches, matched_file_bytes, repo_canonical_root, write_json_line,
};
use crate::cli::search::{collect_scoped_paths, SearchArgs};

/// Size (bytes) of a scoped file with no match, for the summary
/// `bytes_searched` stat. Read entirely from the in-memory index: overlay
/// content length, else the live base-segment doc's `size_bytes`.
///
/// A scoped path absent from the index (an untracked file that happens to be in
/// scope) contributes 0: st never actually searched its content (it is not
/// indexed), so counting its on-disk size would overstate what was searched.
/// This also avoids an `fs::metadata` syscall per unmatched-scoped file, which
/// on a large scope with few matches was O(files) stats just to fill this stat.
fn get_file_size(snap: &crate::index::IndexSnapshot, path: &std::path::Path) -> usize {
    if let Some(doc) = snap.overlay.get_doc_by_path(path) {
        return doc.content.len();
    }
    if let Some(doc_ids) = snap.base.path_doc_ids.get(path) {
        for &global_id in doc_ids {
            if !snap.delete_set.contains(global_id) {
                let seg_idx = snap
                    .segment_base_ids()
                    .partition_point(|&b| b <= global_id)
                    .saturating_sub(1);
                if seg_idx < snap.base_segments().len() {
                    let base = snap.segment_base_ids()[seg_idx];
                    if let Some(local_id) = global_id.checked_sub(base) {
                        if let Some(doc_entry) = snap.base_segments()[seg_idx].get_doc(local_id) {
                            return doc_entry.size_bytes as usize;
                        }
                    }
                }
            }
        }
    }
    0
}

/// Emit rg-compatible NDJSON for all matches: begin/match.../end per file + summary.
pub(in crate::cli) fn render_json(
    index: &Index,
    config: &Config,
    matches: &[crate::SearchMatch],
    files: &std::collections::HashMap<std::path::PathBuf, crate::search::MatchedFile>,
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
    let canonical_root = repo_canonical_root(config);

    for (path, match_lines) in &by_file {
        let file_start = Instant::now();
        let Some(file_content) =
            matched_file_bytes(files, config, &canonical_root, path, args.quiet)
        else {
            continue;
        };
        // Raw (pre-normalize) length for bytes_searched; falls back to the
        // normalized length when the file came from the disk-read path.
        let raw_len = files
            .get(path)
            .map(|mf| mf.raw_len as usize)
            .unwrap_or(file_content.len());
        total_bytes_searched += raw_len;
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
                    file_content.len(),
                    file_bytes_printed,
                    file_matched_lines,
                    file_total_matches
                )
            }
        })
        .to_string();
        write_json_line(&mut out, &end)?;
    }

    let snap = index.snapshot();
    for path in scoped_paths {
        if by_file.contains_key(&path) {
            continue;
        }
        total_bytes_searched += get_file_size(&snap, &path);
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
