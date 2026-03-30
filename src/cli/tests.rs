use std::fs;

use clap::Parser;

use crate::index::Index;
use crate::{Config, SearchOptions};

use super::{manage::cmd_index, manage::cmd_update, Cli, ManageCommand};

#[test]
fn search_works_without_subcommand() {
    let cli = Cli::try_parse_from(["st", "fn_hello"]).expect("parse failed");
    assert!(cli.command.is_none());
    assert_eq!(cli.pattern.as_deref(), Some("fn_hello"));
}

#[test]
fn fixed_strings_short_flag_is_capital_f() {
    let cli = Cli::try_parse_from(["st", "-F", "fn.hello"]).expect("parse failed");
    assert!(cli.fixed_strings);
    assert_eq!(cli.pattern.as_deref(), Some("fn.hello"));
}

#[test]
fn case_sensitivity_flags_follow_ripgrep_names() {
    let cli = Cli::try_parse_from(["st", "-s", "-i", "pattern"]).expect("parse failed");
    assert!(cli.ignore_case, "last flag should win");
    assert!(!cli.case_sensitive, "overridden flag should be cleared");
}

#[test]
fn files_with_matches_short_flag_is_lowercase_l() {
    let cli = Cli::try_parse_from(["st", "-l", "pattern"]).expect("parse failed");
    assert!(cli.files_with_matches);
}

#[test]
fn output_mode_flags_use_last_one_wins() {
    let cli = Cli::try_parse_from(["st", "--json", "-c", "pattern"]).expect("parse failed");
    assert!(cli.count, "last output mode should win");
    assert!(!cli.json, "overridden output mode should be cleared");

    let cli = Cli::try_parse_from(["st", "-c", "-l", "pattern"]).expect("parse failed");
    assert!(cli.files_with_matches, "last output mode should win");
    assert!(!cli.count, "overridden output mode should be cleared");

    let cli = Cli::try_parse_from(["st", "-l", "--files-without-match", "pattern"])
        .expect("parse failed");
    assert!(cli.files_without_match, "last output mode should win");
    assert!(
        !cli.files_with_matches,
        "overridden output mode should be cleared"
    );

    let cli =
        Cli::try_parse_from(["st", "-c", "--count-matches", "pattern"]).expect("parse failed");
    assert!(cli.count_matches, "last output mode should win");
    assert!(!cli.count, "overridden output mode should be cleared");
}

#[test]
fn context_flag_sets_both_before_and_after() {
    let cli = Cli::try_parse_from(["st", "-C", "3", "pattern"]).expect("parse failed");
    assert_eq!(cli.context, Some(3));
}

#[test]
fn line_number_and_filename_aliases_match_ripgrep() {
    let cli = Cli::try_parse_from(["st", "-N", "-n", "-I", "-H", "pattern"]).expect("parse failed");
    assert!(cli.line_number, "last line-number flag should win");
    assert!(
        !cli.no_line_number,
        "overridden no-line-number should be cleared"
    );
    assert!(cli.with_filename, "last filename flag should win");
    assert!(!cli.no_filename, "overridden no-filename should be cleared");
}

#[test]
fn line_regexp_overrides_word_regexp() {
    let cli = Cli::try_parse_from(["st", "-w", "-x", "pattern"]).expect("parse failed");
    assert!(cli.line_regexp, "last boundary mode should win");
    assert!(
        !cli.word_regexp,
        "overridden boundary mode should be cleared"
    );
}

#[test]
fn manage_index_subcommand_still_routes_correctly() {
    let cli = Cli::try_parse_from(["st", "index"]).expect("parse failed");
    assert!(cli.pattern.is_none());
    assert!(matches!(cli.command, Some(ManageCommand::Index { .. })));
}

#[test]
fn cmd_index_rebuilds_existing_index_without_force() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    fs::create_dir_all(repo.path().join("src")).unwrap();
    let file = repo.path().join("src/main.rs");
    fs::write(&file, "fn first_version() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };

    assert_eq!(cmd_index(config.clone(), false, false, true), 0);

    fs::write(&file, "fn second_version() {}\n").unwrap();
    assert_eq!(cmd_index(config.clone(), false, false, true), 0);

    let index = Index::open(config.clone()).unwrap();
    let opts = SearchOptions::default();
    let first = index.search("first_version", &opts).unwrap();
    let second = index.search("second_version", &opts).unwrap();

    assert!(first.is_empty(), "old content should be gone after rebuild");
    assert_eq!(
        second.len(),
        1,
        "new content should be indexed after rebuild"
    );
}

#[test]
fn render_with_context_emits_separator_between_blocks() {
    use std::fs;
    let dir = tempfile::TempDir::new().unwrap();

    // 20-line file; matches on lines 3 and 18 (far enough apart that context=2 creates two separate blocks).
    let content: String = (1..=20)
        .map(|i| {
            if i == 3 || i == 18 {
                format!("target_token line {i}\n")
            } else {
                format!("other line {i}\n")
            }
        })
        .collect();
    let path = dir.path().join("sample.rs");
    fs::write(&path, &content).unwrap();

    let matches = vec![
        crate::SearchMatch {
            path: std::path::PathBuf::from("sample.rs"),
            line_number: 3,
            line_content: b"target_token line 3".to_vec(),
            byte_offset: 0,
            submatch_start: 0,
            submatch_end: "target_token".len(),
        },
        crate::SearchMatch {
            path: std::path::PathBuf::from("sample.rs"),
            line_number: 18,
            line_content: b"target_token line 18".to_vec(),
            byte_offset: 0,
            submatch_start: 0,
            submatch_end: "target_token".len(),
        },
    ];

    let config = Config {
        repo_root: dir.path().to_path_buf(),
        ..Config::default()
    };

    let args = super::search::SearchArgs {
        pattern: "target_token".to_string(),
        after_context: 2,
        before_context: 2,
        ..super::search::SearchArgs::default()
    };

    let mut buf = Vec::<u8>::new();
    super::render::render_with_context_to(&config, &matches, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();

    // Should contain a -- separator between the two non-contiguous context blocks.
    // Block 1: lines 1-5 (around match at line 3)
    // Block 2: lines 16-20 (around match at line 18)
    // Gap between: lines 6-15 are not printed.
    assert!(
        output.contains("--\n"),
        "Expected '--' separator between context blocks, got:\n{output}"
    );

    // Match lines should use ':' separator.
    assert!(
        output.contains(":target_token line 3"),
        "Expected ':' for match line"
    );
    assert!(
        output.contains(":target_token line 18"),
        "Expected ':' for match line"
    );

    // Context lines should use '-' separator.
    assert!(
        output.contains("-other line 1") || output.contains("-other line 2"),
        "Expected '-' for context lines before first match"
    );
}

#[test]
fn json_output_is_ndjson_with_type_envelope() {
    let m = crate::SearchMatch {
        path: std::path::PathBuf::from("src/foo.rs"),
        line_number: 5,
        line_content: b"fn foo() {}".to_vec(),
        byte_offset: 3,
        submatch_start: 3,
        submatch_end: 6,
    };
    let line = super::render::format_match_json(&m);
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("must be valid JSON");
    assert_eq!(parsed["type"], "match");
    assert_eq!(parsed["data"]["line_number"], 5);
    assert_eq!(parsed["data"]["lines"]["text"], "fn foo() {}\n");
    assert_eq!(parsed["data"]["path"]["text"], "src/foo.rs");
}

#[test]
fn cmd_update_on_repo_with_no_commits() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    // Initialize git repo with no commits.
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init"])
        .output()
        .unwrap();

    // Create a file and build the index.
    fs::write(repo.path().join("hello.rs"), "fn hello() {}\n").unwrap();
    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    assert_eq!(cmd_index(config.clone(), false, false, true), 0);

    // cmd_update should not crash on a repo with no commits.
    // git diff HEAD fails, but we fall through to untracked file detection.
    let code = cmd_update(config, false, true);
    assert_ne!(
        code, 2,
        "cmd_update should not error on repo with no commits"
    );
}

#[test]
fn max_file_size_is_clamped_to_1gb() {
    // Use clamp_max_file_size() directly rather than setting the env var.
    // std::env::set_var is not thread-safe: the test harness runs tests in
    // parallel, so a set_var / remove_var pair in one test can affect any
    // concurrent test that reads SYNTEXT_MAX_FILE_SIZE from the environment,
    // causing non-deterministic failures. Testing the inner function avoids
    // the global process state entirely.
    let result = super::clamp_max_file_size(Some(2_147_483_648)); // 2 GiB raw
    assert_eq!(
        result,
        super::MAX_FILE_SIZE_CEILING,
        "value above 1 GiB must be clamped to MAX_FILE_SIZE_CEILING"
    );
    // The +1 used in commit_batch must not overflow after clamping.
    assert!(
        result.checked_add(1).is_some(),
        "clamped value + 1 must not overflow"
    );
}

#[test]
fn line_regexp_wraps_the_entire_pattern() {
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        line_regexp: true,
        ..super::search::SearchArgs::default()
    };
    assert_eq!(super::search::build_effective_pattern(&args), "^(?:foo)$");
}

#[test]
fn max_count_applies_per_file() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    fs::write(repo.path().join("one.rs"), "foo\nfoo\n").unwrap();
    fs::write(repo.path().join("two.rs"), "foo\nfoo\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    assert_eq!(cmd_index(config.clone(), false, false, true), 0);

    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        max_count: Some(1),
        ..super::search::SearchArgs::default()
    };

    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert_eq!(results.len(), 2, "expected one match from each file");
    assert_eq!(
        results
            .iter()
            .filter(|m| m.path == std::path::Path::new("one.rs"))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|m| m.path == std::path::Path::new("two.rs"))
            .count(),
        1
    );
}
