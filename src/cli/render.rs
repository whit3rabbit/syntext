//! Output rendering: flat, heading, invert-match, context, and JSON formats.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::path_util::path_bytes;
use crate::Config;

use super::search::{build_effective_pattern, SearchArgs};
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

pub(super) fn render_invert_match(
    config: &Config,
    candidate_matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> io::Result<i32> {
    // NOTE: render_invert_match only inverts within files that the index identifies
    // as candidates (files containing the pattern). When the pattern appears in no
    // files, this returns exit 1 with no output. True corpus-wide invert-match would
    // require walking all indexed files regardless of candidate set, which is a
    // known v1 limitation.
    use std::collections::BTreeSet;

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

    let files: BTreeSet<_> = candidate_matches
        .iter()
        .map(|m| config.repo_root.join(&m.path))
        .collect();

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut found_any = false;
    for abs_path in &files {
        let rel_path = abs_path.strip_prefix(&config.repo_root).unwrap_or(abs_path);

        #[cfg(unix)]
        let pre_open_meta = match std::fs::metadata(abs_path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let mut file = match crate::index::open_readonly_nofollow(abs_path) {
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

        for_each_line(file_bytes.as_ref(), |line_num, _line_start, line| {
            if !re.is_match(line) {
                found_any = true;
                if !args.quiet {
                    let _ = write_formatted_line(
                        &mut out,
                        args.no_filename,
                        args.no_line_number,
                        rel_path,
                        line_num as usize,
                        b':',
                        line,
                    );
                }
            }
        });
    }

    Ok(if found_any { 0 } else { 1 })
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

        // Print a file-level separator between files (rg behavior: -- between files too).
        if !first_file {
            writeln!(out, "--")?;
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
                args.no_filename,
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
pub(super) fn format_match_json(m: &crate::SearchMatch) -> String {
    let mut line_with_newline = m.line_content.clone();
    line_with_newline.push(b'\n');
    let submatch = &m.line_content[m.submatch_start..m.submatch_end];

    serde_json::json!({
        "type": "match",
        "data": {
            "path": json_data(path_bytes(&m.path).as_ref()),
            "lines": json_data(&line_with_newline),
            "line_number": m.line_number,
            "absolute_offset": m.byte_offset,
            "submatches": [{
                "match": json_data(submatch),
                "start": m.submatch_start,
                "end": m.submatch_end
            }]
        }
    })
    .to_string()
}

/// Emit rg-compatible NDJSON for all matches: begin/match.../end per file + summary.
pub(super) fn render_json(matches: &[crate::SearchMatch]) -> io::Result<()> {
    use std::collections::BTreeMap;

    let mut by_file: BTreeMap<PathBuf, Vec<&crate::SearchMatch>> = BTreeMap::new();
    for m in matches {
        by_file.entry(m.path.clone()).or_default().push(m);
    }

    let total_matches: usize = matches.len();
    let zero_stats = serde_json::json!({
        "elapsed": {"secs": 0, "nanos": 0, "human": "0s"},
        "searches": 1,
        "searches_with_match": if total_matches > 0 { 1 } else { 0 },
        "bytes_searched": 0,
        "bytes_printed": 0,
        "matched_lines": total_matches,
        "matches": total_matches
    });

    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (path, file_matches) in &by_file {
        // begin
        writeln!(
            out,
            "{}",
            serde_json::json!({"type":"begin","data":{"path": json_data(path_bytes(path).as_ref())}})
        )?;
        // match lines
        for m in file_matches {
            writeln!(out, "{}", format_match_json(m))?;
        }
        // end
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "type": "end",
                "data": {
                    "path": json_data(path_bytes(path).as_ref()),
                    "binary_offset": null,
                    "stats": zero_stats
                }
            })
        )?;
    }

    // summary
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "type": "summary",
            "data": {
                "elapsed_total": {"secs": 0, "nanos": 0, "human": "0s"},
                "stats": zero_stats
            }
        })
    )?;
    Ok(())
}
