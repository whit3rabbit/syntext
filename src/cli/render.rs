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

// Task 4 not yet implemented: fall back to flat output until context rendering is added.
pub(super) fn render_with_context(
    _config: &Config,
    matches: &[crate::SearchMatch],
    args: &SearchArgs,
) {
    render_flat(matches, args);
}

// Task 5 not yet implemented: fall back to flat output.
pub(super) fn render_json(_config: &Config, matches: &[crate::SearchMatch], args: &SearchArgs) {
    render_flat(matches, args);
}
