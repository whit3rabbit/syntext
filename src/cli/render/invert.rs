//! Inverted match renderer: prints lines that do NOT match the pattern.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use super::color::write_styled;
use super::{
    compile_output_regex, json_data, json_elapsed, json_stats, read_matched_file,
    repo_canonical_root, write_formatted_line, write_json_line, ColorStyles,
};
use crate::cli::search::{collect_scoped_paths, SearchArgs};

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

    // Bounded auto-update: keep the invert scan's scoped-path listing
    // consistent with a normal search, so a freshly created file is included
    // instead of only showing up after a manual `st update`. See
    // `catchup::run_bounded_auto_update` for the full error-handling contract.
    // Same notice/quiet gating as `cmd_search`: still-stale-after-update spawns
    // the detached async catch-up (after the scan below has finished writing
    // its own output, so the extra process never delays or reorders it),
    // regardless of whether the stderr notice itself was suppressed by
    // `--quiet`.
    let needs_async_catchup =
        crate::cli::catchup::run_bounded_auto_update(index, config, args.quiet);

    let result = render_invert_match_scan(index, config, args, &re, total_start);

    if needs_async_catchup {
        crate::cli::catchup::maybe_spawn_async_catchup(config);
    }

    result
}

fn render_invert_match_scan(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
    re: &regex::bytes::Regex,
    total_start: Instant,
) -> io::Result<i32> {
    let files = collect_scoped_paths(index, config, args);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let styles = ColorStyles::default();
    let mut found_any = false;
    let mut files_without_selected = Vec::new();
    let mut total_bytes_searched = 0usize;
    let mut total_bytes_printed = 0usize;
    let mut total_matched_lines = 0usize;
    let total_searches = files.len();
    let mut searches_with_match = 0usize;
    let mut counts: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut matched_files = Vec::new();
    let canonical_root = repo_canonical_root(config);
    for rel_path in &files {
        let file_start = Instant::now();
        let mut selected_in_file = 0usize;
        let mut file_selected = Vec::new();

        let Some(raw_bytes) = read_matched_file(config, &canonical_root, rel_path, args.quiet)
        else {
            continue;
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
                        super::FormatOpts {
                            no_path: args.no_filename,
                            no_num: args.no_line_number,
                            null: args.null,
                            color: args.color,
                        },
                        rel_path.as_path(),
                        line_num as usize,
                        b':',
                        line,
                        &[],
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

    let sep = if args.null { b'\0' } else { b'\n' };
    if args.files_with_matches {
        for path in matched_files {
            write_styled(
                &mut out,
                args.color,
                styles.path,
                path_bytes(&path).as_ref(),
            )?;
            out.write_all(&[sep])?;
        }
    } else if args.files_without_match {
        for path in &files_without_selected {
            write_styled(&mut out, args.color, styles.path, path_bytes(path).as_ref())?;
            out.write_all(&[sep])?;
        }
    } else if args.count {
        for (path, count) in counts {
            if args.no_filename {
                writeln!(out, "{count}")?;
            } else {
                write_styled(
                    &mut out,
                    args.color,
                    styles.path,
                    path_bytes(&path).as_ref(),
                )?;
                let count_sep = if args.null { b'\0' } else { b':' };
                out.write_all(&[count_sep])?;
                writeln!(out, "{count}")?;
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
