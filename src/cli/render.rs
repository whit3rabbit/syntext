//! Output rendering: flat, heading, invert-match, context, and JSON formats.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::index::Index;
use crate::path_util::path_bytes;
use crate::Config;

use super::search::{build_effective_pattern, collect_scoped_paths, SearchArgs};
use crate::search::lines::for_each_line;
use crate::search::REGEX_SIZE_LIMIT;

fn write_formatted_line(
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

fn json_data(bytes: &[u8]) -> serde_json::Value {
    if let Ok(text) = std::str::from_utf8(bytes) {
        serde_json::json!({ "text": text })
    } else {
        serde_json::json!({ "bytes": crate::base64::encode(bytes) })
    }
}

fn json_stats(
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

fn json_elapsed(elapsed: Duration) -> serde_json::Value {
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

fn json_submatches(re: &regex::bytes::Regex, line: &[u8]) -> Vec<serde_json::Value> {
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

fn json_line_message(
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

fn write_json_line(out: &mut dyn Write, line: &str) -> io::Result<usize> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(line.len() + 1)
}

fn read_repo_file_bytes(config: &Config, rel_path: &Path) -> io::Result<Vec<u8>> {
    let abs_path = config.repo_root.join(rel_path);

    #[cfg(unix)]
    let pre_open_meta = std::fs::metadata(&abs_path)?;
    let mut file = crate::index::open_readonly_nofollow(&abs_path)?;
    #[cfg(unix)]
    if !crate::index::verify_fd_matches_stat(&file, &pre_open_meta) {
        return Err(io::Error::other("path changed during verification"));
    }

    let mut raw_content = Vec::new();
    file.read_to_end(&mut raw_content)?;
    Ok(raw_content)
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
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for m in matches {
        write_formatted_line(
            &mut out,
            args.no_filename,
            args.no_line_number,
            &m.path,
            m.line_number as usize,
            b':',
            &m.line_content,
        )?;
    }
    Ok(())
}

pub(super) fn render_count_matches(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<i32> {
    let pattern = build_effective_pattern(args);
    let re = match regex::bytes::RegexBuilder::new(&pattern)
        .case_insensitive(args.ignore_case)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("st: invalid pattern: {e}");
            return Ok(2);
        }
    };

    let mut per_file: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for m in matches {
        per_file.entry(m.path.clone()).or_insert(0);
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut found_any = false;
    for path in per_file.keys() {
        let abs_path = config.repo_root.join(path);

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
        let mut raw_bytes = Vec::new();
        if file.read_to_end(&mut raw_bytes).is_err() {
            continue;
        }
        let file_bytes = crate::index::normalize_encoding(&raw_bytes, config.verbose);

        let mut count = 0usize;
        for_each_line(file_bytes.as_ref(), |_, _, line| {
            count += re.find_iter(line).count();
        });
        if count == 0 {
            continue;
        }
        found_any = true;
        if args.no_filename {
            writeln!(out, "{count}")?;
        } else {
            out.write_all(path_bytes(path).as_ref())?;
            writeln!(out, ":{count}")?;
        }
    }

    Ok(if found_any { 0 } else { 1 })
}

pub(super) fn render_only_matching(
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
    let unique_paths: std::collections::BTreeSet<_> =
        matches.iter().map(|m| m.path.clone()).collect();
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
    use std::collections::{BTreeMap, BTreeSet};

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
                writeln!(out, "--")?;
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
                    writeln!(out, "--")?;
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

pub(super) fn render_heading(matches: &[crate::SearchMatch], args: &SearchArgs) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
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
        if args.no_line_number {
            out.write_all(&m.line_content)?;
            out.write_all(b"\n")?;
        } else {
            write!(out, "{}:", m.line_number)?;
            out.write_all(&m.line_content)?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn compile_output_regex(args: &SearchArgs) -> io::Result<regex::bytes::Regex> {
    regex::bytes::RegexBuilder::new(&build_effective_pattern(args))
        .case_insensitive(args.ignore_case)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
}

pub(super) fn render_invert_match(
    index: &Index,
    config: &Config,
    args: &SearchArgs,
) -> io::Result<i32> {
    let total_start = Instant::now();
    let pattern = build_effective_pattern(args);
    let re = match regex::bytes::RegexBuilder::new(&pattern)
        .case_insensitive(args.ignore_case)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
    {
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
        let abs_path = config.repo_root.join(rel_path);
        let mut selected_in_file = 0usize;
        let mut file_selected = Vec::new();

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
        let mut raw_bytes = Vec::new();
        if file.read_to_end(&mut raw_bytes).is_err() {
            continue;
        }
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

/// Print matches with surrounding context lines to stdout.
///
/// Lines from context (not the match itself) use `-` as the separator; match lines use `:`.
/// Blocks separated by a gap in line numbers emit a `--` context separator.
pub(super) fn render_with_context(
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
pub(super) fn render_with_context_to(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
    out: &mut dyn std::io::Write,
) -> io::Result<()> {
    use std::collections::{BTreeMap, BTreeSet};

    // Group match line numbers by relative path string.
    let mut by_file: BTreeMap<PathBuf, Vec<u32>> = BTreeMap::new();
    for m in matches {
        by_file
            .entry(m.path.clone())
            .or_default()
            .push(m.line_number);
    }

    let before = args.before_context;
    let after = args.after_context;
    let grouped_heading = args.heading && (by_file.len() > 1 || !args.no_filename);
    let suppress_path_prefix = args.heading;

    let mut first_file = true;
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

        // Ripgrep uses blank lines between headed file groups, but `--` for flat file separators.
        if !first_file {
            if grouped_heading {
                writeln!(out)?;
            } else {
                writeln!(out, "--")?;
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
                    writeln!(out, "--")?;
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

/// Format a single SearchMatch as a rg-compatible JSON `match` message.
/// Returns a single NDJSON line (no trailing newline).
/// Emit rg-compatible NDJSON for all matches: begin/match.../end per file + summary.
pub(super) fn render_json(
    index: &Index,
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<()> {
    let total_start = Instant::now();
    let re = compile_output_regex(args)?;
    let mut by_file: BTreeMap<PathBuf, Vec<u32>> = BTreeMap::new();
    for m in matches {
        by_file
            .entry(m.path.clone())
            .or_default()
            .push(m.line_number);
    }
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
