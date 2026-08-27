//! Unit tests for the stdin filter-mode guard and per-line invert.

use super::*;
use std::path::Path;

fn args(pattern: &str, paths: &[&str]) -> SearchArgs {
    SearchArgs {
        pattern: pattern.to_string(),
        paths: paths.iter().map(PathBuf::from).collect(),
        // Unit tests of the guard assume the CLI-binary context.
        allow_implicit_stdin: true,
        ..SearchArgs::default()
    }
}

#[test]
fn no_paths_uses_stdin_only_when_searchable() {
    assert_eq!(
        decide_stdin(true, &args("pat", &[])),
        StdinDecision::UseStdin
    );
    // /dev/null, socket, tty, or closed stdin: stay on the repo-index path.
    assert_eq!(
        decide_stdin(false, &args("pat", &[])),
        StdinDecision::NotStdin
    );
}

#[test]
fn explicit_dash_always_wins() {
    // `-` means stdin regardless of what is attached, matching ripgrep (it
    // reads stdin for `-` even when stdin is /dev/null).
    assert_eq!(
        decide_stdin(false, &args("pat", &["-"])),
        StdinDecision::UseStdin
    );
    assert_eq!(
        decide_stdin(true, &args("pat", &["-"])),
        StdinDecision::UseStdin
    );
}

#[test]
fn dash_mixed_with_paths_searches_both() {
    assert_eq!(
        decide_stdin(true, &args("pat", &["-", "src"])),
        StdinDecision::StdinPlusPaths
    );
    assert_eq!(
        decide_stdin(true, &args("pat", &["src", "-"])),
        StdinDecision::StdinPlusPaths
    );
    assert!(dash_precedes_real_paths(&args("pat", &["-", "src"])));
    assert!(!dash_precedes_real_paths(&args("pat", &["src", "-"])));
}

#[test]
fn real_paths_beat_stdin() {
    // Explicit path arguments override stdin (ripgrep rule).
    assert_eq!(
        decide_stdin(true, &args("pat", &["src"])),
        StdinDecision::NotStdin
    );
}

#[test]
fn implicit_stdin_requires_cli_context() {
    // In-process callers (unit tests, library use) never get implicit stdin
    // mode; only the CLI binary entry opts in.
    let mut a = args("pat", &[]);
    a.allow_implicit_stdin = false;
    assert_eq!(decide_stdin(true, &a), StdinDecision::NotStdin);
    // The explicit dash still means stdin without the flag.
    a.paths = vec![PathBuf::from("-")];
    assert_eq!(decide_stdin(false, &a), StdinDecision::UseStdin);
}

#[test]
fn empty_pattern_and_dashless_l_still_filter_the_stream() {
    // rg `cmd | rg ''` prints every line, and `cmd | rg --files-without-match
    // pat` lists `<stdin>` when the stream does not match: neither an empty
    // pattern nor -L keeps the implicit pipe out of stdin mode.
    assert_eq!(decide_stdin(true, &args("", &[])), StdinDecision::UseStdin);
    let mut a = args("pat", &[]);
    a.files_without_match = true;
    assert_eq!(decide_stdin(true, &a), StdinDecision::UseStdin);
}

#[test]
fn sym_routes_to_the_index_even_with_a_pipe() {
    let mut a = args("pat", &[]);
    a.sym = Some("Foo".to_string());
    assert_eq!(decide_stdin(true, &a), StdinDecision::NotStdin);
}

#[test]
fn invert_matches_yields_non_matching_lines() {
    let re = regex::bytes::Regex::new("b").unwrap();
    let matches = invert_matches(&re, Path::new(STDIN_LABEL), b"a\nbb\nc\n");
    let lines: Vec<&str> = matches
        .iter()
        .map(|m| std::str::from_utf8(&m.line_content).unwrap())
        .collect();
    assert_eq!(lines, vec!["a", "c"]);
    assert_eq!(matches[0].line_number, 1);
    assert_eq!(matches[1].line_number, 3);
    assert_eq!(matches[0].byte_offset, 0);
    assert_eq!(matches[1].byte_offset, 5);
}

#[test]
fn invert_matches_covers_binary_content() {
    // The per-line inverter no longer skips NUL-containing streams; the
    // rg binary policy (notice + exit 0) is applied by collect_stdin.
    let re = regex::bytes::Regex::new("b").unwrap();
    let matches = invert_matches(&re, Path::new(STDIN_LABEL), b"a\0z\nb\n");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line_number, 1);
    assert_eq!(matches[0].line_content, b"a\0z");
}
