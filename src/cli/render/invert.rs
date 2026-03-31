//! Inverted match renderer: prints lines that do NOT match the pattern.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use crate::cli::search::{collect_scoped_paths, SearchArgs};
use super::{compile_output_regex, json_data, json_elapsed, json_stats, read_repo_file_bytes, write_formatted_line, write_json_line};

pub(in crate::cli) fn render_invert_match(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
) -> io::Result<i32> {
    let total_start = Instant::now();
    let re = match compile_output_regex(args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("st: invalid pattern: {e}");
            return Ok(2);
        }
    };

    let files = collect_scoped_paths(index, config, args);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut found_any = false;
    let mut files_without_selected = Vec::new();
    let mut total_bytes_searched = 0usize;
    let mut total_bytes_printed = 0usize;
    let mut total_matched_lines = 0usize;
    let total_searches = files.len();
    let mut searches_with_match = 0usize;
    let mut counts: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut matched_files = Vec::new();
    for rel_path in &files {
        let file_start = Instant::now();
        let mut selected_in_file = 0usize;
        let mut file_selected = Vec::new();

        let raw_bytes = match read_repo_file_bytes(config, rel_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        total_bytes_searched += raw_bytes.len();
        let file_bytes = crate::index::normalize_encoding(&raw_bytes, config.verbose);

        for_each_line(file_bytes.as_ref(), |line_num, line_start, line| {
            if args
                .max_count
                .is_some_and(|limit| selected_in_file >= limit)
            {
                return;
            }
            if !re.is_match(line) {
                found_any = true;
                selected_in_file += 1;
                if args.quiet {
                    return;
                }
                if args.json {
                    file_selected.push((line_num as usize, line_start, line.to_vec()));
                } else if !args.files_with_matches && !args.files_without_match && !args.count {
                    let _ = write_formatted_line(
                        &mut out,
                        args.no_filename,
                        args.no_line_number,
                        rel_path.as_path(),
                        line_num as usize,
                        b':',
                        line,
                    );
                }
            }
        });

        if args.quiet && found_any {
            return Ok(0);
        }
        if selected_in_file == 0 {
            if args.files_without_match {
                files_without_selected.push(rel_path.clone());
            }
            continue;
        }
        total_matched_lines += selected_in_file;
        searches_with_match += 1;
        if args.files_with_matches {
            matched_files.push(rel_path.clone());
        } else if args.count {
            counts.insert(rel_path.clone(), selected_in_file);
        } else if args.json {
            let mut file_bytes_printed = 0usize;
            let begin = serde_json::json!({"type":"begin","data":{"path": json_data(path_bytes(rel_path).as_ref())}})
                .to_string();
            file_bytes_printed += write_json_line(&mut out, &begin)?;
            for (line_number, absolute_offset, line) in file_selected {
                let mut line_with_newline = line;
                line_with_newline.push(b'\n');
                let match_line = serde_json::json!({
                    "type": "match",
                    "data": {
                        "path": json_data(path_bytes(rel_path).as_ref()),
                        "lines": json_data(&line_with_newline),
                        "line_number": line_number,
                        "absolute_offset": absolute_offset,
                        "submatches": []
                    }
                })
                .to_string();
                file_bytes_printed += write_json_line(&mut out, &match_line)?;
            }
            total_bytes_printed += file_bytes_printed;
            let end = serde_json::json!({
                "type": "end",
                "data": {
                    "path": json_data(path_bytes(rel_path).as_ref()),
                    "binary_offset": null,
                    "stats": json_stats(
                        file_start.elapsed(),
                        1,
                        1,
                        raw_bytes.len(),
                        file_bytes_printed,
                        selected_in_file,
                        0
                    )
                }
            })
            .to_string();
            write_json_line(&mut out, &end)?;
        }
    }

    if args.files_with_matches {
        for path in matched_files {
            out.write_all(path_bytes(&path).as_ref())?;
            out.write_all(b"\n")?;
        }
    } else if args.files_without_match {
        for path in &files_without_selected {
            out.write_all(path_bytes(path).as_ref())?;
            out.write_all(b"\n")?;
        }
    } else if args.count {
        for (path, count) in counts {
            if args.no_filename {
                writeln!(out, "{count}")?;
            } else {
                out.write_all(path_bytes(&path).as_ref())?;
                writeln!(out, ":{count}")?;
            }
        }
    } else if args.json {
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
                        0
                    )
                }
            })
        )?;
    }

    let mode_found_any = if args.files_without_match {
        !files_without_selected.is_empty()
    } else {
        found_any
    };

    Ok(if mode_found_any { 0 } else { 1 })
}
