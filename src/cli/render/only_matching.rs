//! Only-matching output renderer: prints only the matched portions of lines.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::path_util::path_bytes;
use crate::search::lines::for_each_line;
use crate::Config;

use crate::cli::search::SearchArgs;
use super::{compile_output_regex, write_formatted_line};

pub(in crate::cli) fn render_only_matching(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    let re = compile_output_regex(args)?;
    if args.before_context > 0 || args.after_context > 0 {
        return render_only_matching_with_context(config, matches, args, &re);
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let unique_paths: BTreeSet<_> = matches.iter().map(|m| m.path.clone()).collect();
    let grouped_heading = args.heading && (unique_paths.len() > 1 || !args.no_filename);
    let suppress_path_prefix = args.heading;
    let mut current_path: Option<&Path> = None;

    for m in matches {
        let path_changed = current_path != Some(m.path.as_path());
        if path_changed && grouped_heading {
            if current_path.is_some() {
                writeln!(out)?;
            }
            if !args.no_filename {
                out.write_all(path_bytes(&m.path).as_ref())?;
                out.write_all(b"\n")?;
            }
        }
        current_path = Some(m.path.as_path());

        for matched in re.find_iter(&m.line_content) {
            if matched.start() == matched.end() {
                continue;
            }
            write_formatted_line(
                &mut out,
                suppress_path_prefix || args.no_filename,
                args.no_line_number,
                &m.path,
                m.line_number as usize,
                b':',
                &m.line_content[matched.start()..matched.end()],
            )?;
        }
    }

    Ok(())
}

fn render_only_matching_with_context(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    re: &regex::bytes::Regex,
) -> io::Result<()> {
    let mut by_file: BTreeMap<PathBuf, Vec<u32>> = BTreeMap::new();
    for m in matches {
        by_file
            .entry(m.path.clone())
            .or_default()
            .push(m.line_number);
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let before = args.before_context;
    let after = args.after_context;
    let mut first_file = true;
    let grouped_heading = args.heading && (by_file.len() > 1 || !args.no_filename);
    let suppress_path_prefix = args.heading;

    for (rel_path, match_lines) in &by_file {
        let abs_path = config.repo_root.join(rel_path);

        #[cfg(unix)]
        let pre_open_meta = match std::fs::metadata(&abs_path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let mut file = match crate::index::open_readonly_nofollow(&abs_path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        #[cfg(unix)]
        if !crate::index::verify_fd_matches_stat(&file, &pre_open_meta) {
            continue;
        }
        let mut raw_content = Vec::new();
        if file.read_to_end(&mut raw_content).is_err() {
            continue;
        }
        let file_content = crate::index::normalize_encoding(&raw_content, config.verbose);
        let mut file_lines: Vec<Vec<u8>> = Vec::new();
        for_each_line(file_content.as_ref(), |_, _, line| {
            file_lines.push(line.to_vec())
        });

        let match_set: BTreeSet<usize> = match_lines
            .iter()
            .map(|&n| (n as usize).saturating_sub(1))
            .collect();
        let mut to_print: BTreeSet<usize> = BTreeSet::new();
        for &mi in &match_set {
            let start = mi.saturating_sub(before);
            let end = (mi + after).min(file_lines.len().saturating_sub(1));
            for idx in start..=end {
                to_print.insert(idx);
            }
        }

        if !first_file && !to_print.is_empty() {
            if grouped_heading {
                writeln!(out)?;
            } else {
                writeln!(out, "{}", args.context_separator)?;
            }
        }
        if grouped_heading && !to_print.is_empty() && !args.no_filename {
            out.write_all(path_bytes(rel_path).as_ref())?;
            out.write_all(b"\n")?;
        }
        first_file = false;

        let mut prev: Option<usize> = None;
        for &idx in &to_print {
            if let Some(p) = prev {
                if idx > p + 1 {
                    writeln!(out, "{}", args.context_separator)?;
                }
            }

            let line_num = idx + 1;
            let content = file_lines.get(idx).map(Vec::as_slice).unwrap_or_default();
            if match_set.contains(&idx) {
                for matched in re.find_iter(content) {
                    if matched.start() == matched.end() {
                        continue;
                    }
                    write_formatted_line(
                        &mut out,
                        suppress_path_prefix || args.no_filename,
                        args.no_line_number,
                        rel_path,
                        line_num,
                        b':',
                        &content[matched.start()..matched.end()],
                    )?;
                }
            } else {
                write_formatted_line(
                    &mut out,
                    suppress_path_prefix || args.no_filename,
                    args.no_line_number,
                    rel_path,
                    line_num,
                    b'-',
                    content,
                )?;
            }

            prev = Some(idx);
        }
    }

    Ok(())
}
