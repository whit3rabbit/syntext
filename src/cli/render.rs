//! Output rendering: flat, heading, invert-match, context, and JSON formats.

use crate::Config;

use super::search::{SearchArgs, build_effective_pattern};

pub(super) fn render_flat(matches: &[crate::SearchMatch], args: &SearchArgs) {
    for m in matches {
        let path = m.path.display();
        if args.no_filename && args.no_line_number {
            println!("{}", m.line_content);
        } else if args.no_filename {
            println!("{}:{}", m.line_number, m.line_content);
        } else if args.no_line_number {
            println!("{path}:{}", m.line_content);
        } else {
            println!("{path}:{}:{}", m.line_number, m.line_content);
        }
    }
}

pub(super) fn render_heading(matches: &[crate::SearchMatch], args: &SearchArgs) {
    let mut current_path: Option<String> = None;
    for m in matches {
        let path_str = m.path.to_string_lossy().into_owned();
        if current_path.as_deref() != Some(&path_str) {
            if current_path.is_some() {
                println!();
            }
            println!("{path_str}");
            current_path = Some(path_str);
        }
        if args.no_line_number {
            println!("{}", m.line_content);
        } else {
            println!("{}:{}", m.line_number, m.line_content);
        }
    }
}

pub(super) fn render_invert_match(
    config: &Config,
    candidate_matches: &[crate::SearchMatch],
    args: &SearchArgs,
) -> i32 {
    // NOTE: render_invert_match only inverts within files that the index identifies
    // as candidates (files containing the pattern). When the pattern appears in no
    // files, this returns exit 1 with no output. True corpus-wide invert-match would
    // require walking all indexed files regardless of candidate set, which is a
    // known v1 limitation.
    use std::collections::BTreeSet;
    use std::io::BufRead;

    let pattern = build_effective_pattern(args);
    let re = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(args.ignore_case)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rl: invalid pattern: {e}");
            return 2;
        }
    };

    let files: BTreeSet<_> = candidate_matches
        .iter()
        .map(|m| config.repo_root.join(&m.path))
        .collect();

    let mut found_any = false;
    for abs_path in &files {
        let rel_path = abs_path
            .strip_prefix(&config.repo_root)
            .unwrap_or(abs_path);

        let file = match std::fs::File::open(abs_path) {
            Ok(f) => f,
            Err(_) => continue,
        };

        for (idx, line) in std::io::BufReader::new(file).lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if !re.is_match(&line) {
                found_any = true;
                if !args.quiet {
                    let line_num = idx + 1;
                    if args.no_filename && args.no_line_number {
                        println!("{line}");
                    } else if args.no_filename {
                        println!("{line_num}:{line}");
                    } else if args.no_line_number {
                        println!("{}:{line}", rel_path.display());
                    } else {
                        println!("{}:{line_num}:{line}", rel_path.display());
                    }
                }
            }
        }
    }

    if found_any { 0 } else { 1 }
}

/// Print matches with surrounding context lines to stdout.
///
/// Lines from context (not the match itself) use `-` as the separator; match lines use `:`.
/// Blocks separated by a gap in line numbers emit a `--` context separator.
pub(super) fn render_with_context(
    config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    render_with_context_to(config, matches, args, &mut out);
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
) {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::BufRead;

    // Group match line numbers by relative path string.
    let mut by_file: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for m in matches {
        by_file
            .entry(m.path.to_string_lossy().into_owned())
            .or_default()
            .push(m.line_number);
    }

    let before = args.before_context;
    let after = args.after_context;

    let mut first_file = true;
    for (rel_path_str, match_lines) in &by_file {
        let abs_path = config.repo_root.join(rel_path_str);

        let file_lines: Vec<String> = match std::fs::File::open(&abs_path) {
            Ok(f) => std::io::BufReader::new(f)
                .lines()
                .map(|l| l.unwrap_or_default())
                .collect(),
            Err(_) => continue,
        };

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
            let _ = writeln!(out, "--");
        }
        first_file = false;

        let mut prev: Option<usize> = None;
        for &idx in &to_print {
            // Gap separator between non-contiguous context blocks.
            if let Some(p) = prev {
                if idx > p + 1 {
                    let _ = writeln!(out, "--");
                }
            }

            let line_num = idx + 1;
            let content = file_lines.get(idx).map(String::as_str).unwrap_or("");
            let is_match = match_set.contains(&idx);
            let sep = if is_match { ':' } else { '-' };

            if args.no_filename && args.no_line_number {
                let _ = writeln!(out, "{content}");
            } else if args.no_filename {
                let _ = writeln!(out, "{line_num}{sep}{content}");
            } else if args.no_line_number {
                let _ = writeln!(out, "{rel_path_str}{sep}{content}");
            } else {
                let _ = writeln!(out, "{rel_path_str}{sep}{line_num}{sep}{content}");
            }

            prev = Some(idx);
        }
    }
}

// Task 5 not yet implemented: fall back to flat output.
pub(super) fn render_json(_config: &Config, matches: &[crate::SearchMatch], args: &SearchArgs) {
    render_flat(matches, args);
}
