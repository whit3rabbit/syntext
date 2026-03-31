use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use clap::Parser;

use crate::index::Index;
use crate::{Config, SearchOptions};

use super::{manage::cmd_index, manage::cmd_update, Cli, ManageCommand};

/// Build a temporary index from a list of (relative_path, content) pairs.
/// Returns (repo_dir, index_dir, config) — caller must keep all three alive.
fn build_index_for_files(
    files: &[(&str, &str)],
) -> (tempfile::TempDir, tempfile::TempDir, Config) {
    let repo = tempfile::TempDir::new().unwrap();
    let idx = tempfile::TempDir::new().unwrap();
    for (name, content) in files {
        let p = repo.path().join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }
    let config = Config {
        index_dir: idx.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    assert_eq!(cmd_index(config.clone(), false, false, true), 0);
    (repo, idx, config)
}

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
fn only_matching_short_flag_is_lowercase_o() {
    let cli = Cli::try_parse_from(["st", "-o", "pattern"]).expect("parse failed");
    assert!(cli.only_matching);
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
    let mut rebuild_status = cmd_index(config.clone(), false, false, true);
    for _ in 0..10 {
        if rebuild_status == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
        rebuild_status = cmd_index(config.clone(), false, false, true);
    }
    assert_eq!(rebuild_status, 0);

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
    drop(index);
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
    drop(index);
}

// --- No-op rg-compatibility flag parse tests ---

#[test]
fn smart_case_short_flag_is_capital_s() {
    let cli = Cli::try_parse_from(["st", "-S", "pat"]).unwrap();
    assert!(cli.smart_case);
}

#[test]
fn noops_color_and_colors_are_accepted() {
    let cli = Cli::try_parse_from(["st", "--color", "never", "pat"]).unwrap();
    assert_eq!(cli.color.as_deref(), Some("never"));

    let cli = Cli::try_parse_from(["st", "--colors", "match:fg:red", "pat"]).unwrap();
    assert_eq!(cli.colors, ["match:fg:red"]);
}

#[test]
fn noops_ignore_and_discovery_flags_are_accepted() {
    let cli = Cli::try_parse_from([
        "st", "--no-ignore", "--no-ignore-vcs", "--hidden", "--follow", "pat",
    ])
    .unwrap();
    assert!(cli.no_ignore && cli.no_ignore_vcs && cli.hidden && cli.follow);
}

#[test]
fn noops_unrestricted_counts_repetitions() {
    let cli = Cli::try_parse_from(["st", "-uuu", "pat"]).unwrap();
    assert_eq!(cli.unrestricted, 3);
}

#[test]
fn noops_sort_and_sortr_are_accepted() {
    let cli = Cli::try_parse_from(["st", "--sort", "path", "pat"]).unwrap();
    assert_eq!(cli.sort.as_deref(), Some("path"));

    let cli = Cli::try_parse_from(["st", "--sortr", "modified", "pat"]).unwrap();
    assert_eq!(cli.sortr.as_deref(), Some("modified"));
}

#[test]
fn noops_binary_text_encoding_flags_are_accepted() {
    let cli =
        Cli::try_parse_from(["st", "-a", "--binary", "-E", "utf-8", "--crlf", "--null-data", "pat"])
            .unwrap();
    assert!(cli.text && cli.binary && cli.crlf && cli.null_data);
    assert_eq!(cli.encoding.as_deref(), Some("utf-8"));
}

#[test]
fn noops_multiline_engine_size_limits_are_accepted() {
    let cli = Cli::try_parse_from([
        "st",
        "-U",
        "--multiline-dotall",
        "--engine",
        "auto",
        "--dfa-size-limit",
        "128M",
        "--regex-size-limit",
        "64M",
        "pat",
    ])
    .unwrap();
    assert!(cli.multiline && cli.multiline_dotall);
    assert_eq!(cli.engine.as_deref(), Some("auto"));
    assert_eq!(cli.dfa_size_limit.as_deref(), Some("128M"));
    assert_eq!(cli.regex_size_limit.as_deref(), Some("64M"));
}

#[test]
fn noops_pattern_file_type_management_are_accepted() {
    let cli = Cli::try_parse_from([
        "st",
        "-f",
        "/tmp/p.txt",
        "--type-add",
        "mine:*.x",
        "--type-clear",
        "go",
        "--iglob",
        "*.txt",
        "pat",
    ])
    .unwrap();
    assert_eq!(cli.pattern_file, Some(PathBuf::from("/tmp/p.txt")));
    assert_eq!(cli.type_add, ["mine:*.x"]);
    assert_eq!(cli.type_clear, ["go"]);
    assert_eq!(cli.iglob.as_deref(), Some("*.txt"));
}

#[test]
fn noops_threads_mmap_performance_flags_are_accepted() {
    let cli = Cli::try_parse_from(["st", "-j", "4", "--mmap", "pat"]).unwrap();
    assert_eq!(cli.threads, Some(4));
    assert!(cli.mmap);

    let cli = Cli::try_parse_from(["st", "--no-mmap", "pat"]).unwrap();
    assert!(cli.no_mmap);
}

#[test]
fn noops_preprocessing_and_zip_are_accepted() {
    let cli =
        Cli::try_parse_from(["st", "--pre", "decomp", "--pre-glob", "*.gz", "-z", "pat"]).unwrap();
    assert_eq!(cli.pre.as_deref(), Some("decomp"));
    assert_eq!(cli.pre_glob.as_deref(), Some("*.gz"));
    assert!(cli.search_zip);
}

#[test]
fn noops_diagnostics_config_flags_are_accepted() {
    let cli = Cli::try_parse_from([
        "st",
        "--trace",
        "--no-messages",
        "--no-config",
        "--one-file-system",
        "pat",
    ])
    .unwrap();
    assert!(cli.trace && cli.no_messages && cli.no_config && cli.one_file_system);
}

#[test]
fn noops_max_filesize_is_accepted() {
    let cli = Cli::try_parse_from(["st", "--max-filesize", "1M", "pat"]).unwrap();
    assert_eq!(cli.max_filesize.as_deref(), Some("1M"));
}

#[test]
fn null_short_flag_is_zero() {
    let cli = Cli::try_parse_from(["st", "-0", "pat"]).unwrap();
    assert!(cli.null);

    let cli = Cli::try_parse_from(["st", "--null", "pat"]).unwrap();
    assert!(cli.null);
}

#[test]
fn debug_flag_sets_debug_field() {
    let cli = Cli::try_parse_from(["st", "--debug", "pat"]).unwrap();
    assert!(cli.debug);
}

#[test]
fn pretty_short_flag_is_lowercase_p() {
    let cli = Cli::try_parse_from(["st", "-p", "pat"]).unwrap();
    assert!(cli.pretty);
}

// --- Functional flag tests ---

#[test]
fn max_depth_filters_results_by_directory_depth() {
    let (_repo, _idx, config) = build_index_for_files(&[
        ("top.rs", "needle\n"),
        ("sub/mid.rs", "needle\n"),
        ("a/b/deep.rs", "needle\n"),
    ]);
    let index = Index::open(config.clone()).unwrap();

    let args0 = super::search::SearchArgs {
        pattern: "needle".to_string(),
        max_depth: Some(0),
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args0).unwrap();
    let paths: Vec<_> = results.iter().map(|m| m.path.as_path()).collect();
    assert!(paths.contains(&std::path::Path::new("top.rs")));
    assert!(!paths.iter().any(|p| p.components().count() > 1));

    let args1 = super::search::SearchArgs {
        pattern: "needle".to_string(),
        max_depth: Some(1),
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args1).unwrap();
    let paths: Vec<_> = results.iter().map(|m| m.path.as_path()).collect();
    assert!(paths.contains(&std::path::Path::new("top.rs")));
    assert!(paths.contains(&std::path::Path::new("sub/mid.rs")));
    assert!(!paths.contains(&std::path::Path::new("a/b/deep.rs")));

    let args_all = super::search::SearchArgs {
        pattern: "needle".to_string(),
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args_all).unwrap();
    assert_eq!(results.len(), 3);

    drop(index);
}

#[test]
fn column_flag_prepends_column_in_flat_output() {
    let (_repo, _idx, config) = build_index_for_files(&[("src/lib.rs", "fn hello() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "hello".to_string(),
        column: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    // "fn hello() {}" — "hello" starts at byte 3, col = 3+1 = 4
    assert!(
        output.contains("src/lib.rs:1:4:"),
        "expected path:line:col: prefix, got: {output:?}"
    );
    drop(index);
}

#[test]
fn column_flag_in_heading_mode() {
    let (_repo, _idx, config) = build_index_for_files(&[("src/lib.rs", "fn hello() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "hello".to_string(),
        column: true,
        heading: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_heading_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    // Under heading mode: filename on its own line, then line:col:content
    assert!(
        output.contains("1:4:"),
        "expected line:col: in heading output, got: {output:?}"
    );
    drop(index);
}

#[test]
fn vimgrep_output_format() {
    let (_repo, _idx, config) =
        build_index_for_files(&[("a.rs", "fn foo() {}\nfn bar() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        vimgrep: true,
        column: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_vimgrep_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    // "fn foo() {}" — "foo" starts at byte 3, col = 4
    assert!(
        output.contains("a.rs:1:4:"),
        "expected path:line:col: in vimgrep output, got: {output:?}"
    );
    assert!(!output.contains("bar"), "bar should not appear in results for 'foo'");
    drop(index);
}

#[test]
fn replace_substitutes_match_in_output() {
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "fn old_name() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "old_name".to_string(),
        replace: Some("new_name".to_string()),
        no_line_number: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert_eq!(output, "fn new_name() {}\n");
    drop(index);
}

#[test]
fn replace_uses_capture_groups() {
    let (_repo, _idx, config) = build_index_for_files(&[("v.rs", "v = \"1.2.3\"\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: r"(\d+)\.(\d+)\.(\d+)".to_string(),
        replace: Some("$3.$2.$1".to_string()),
        no_line_number: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.contains("3.2.1"),
        "expected reversed version, got: {output:?}"
    );
    drop(index);
}

#[test]
fn byte_offset_prepends_offset_before_line() {
    // "aaa\nfoo\n": 'f' is at absolute byte offset 4
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "aaa\nfoo\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        byte_offset: true,
        no_line_number: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.starts_with("4:"),
        "expected byte offset 4, got: {output:?}"
    );
    drop(index);
}

#[test]
fn trim_strips_leading_whitespace() {
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "   fn foo() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        trim: true,
        no_line_number: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert_eq!(output, "fn foo() {}\n");
    drop(index);
}

#[test]
fn max_columns_skips_long_lines() {
    let (_repo, _idx, config) = build_index_for_files(&[
        ("a.rs", "short_match\n"),
        (
            "b.rs",
            "this_is_a_very_long_line_with_match_here_that_exceeds_the_limit\n",
        ),
    ]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "match".to_string(),
        max_columns: Some(20),
        no_line_number: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("short_match"), "short line should appear");
    assert!(
        !output.contains("this_is_a_very_long"),
        "long line should be skipped"
    );
    drop(index);
}

#[test]
fn context_separator_custom_string() {
    let dir = tempfile::TempDir::new().unwrap();

    let content: String = (1..=20)
        .map(|i| {
            if i == 3 || i == 18 {
                format!("target_token line {i}\n")
            } else {
                format!("other line {i}\n")
            }
        })
        .collect();
    fs::write(dir.path().join("sample.rs"), &content).unwrap();

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
        context_separator: "===".to_string(),
        after_context: 2,
        before_context: 2,
        ..super::search::SearchArgs::default()
    };

    let mut buf = Vec::<u8>::new();
    super::render::render_with_context_to(&config, &matches, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();

    assert!(
        output.contains("===\n"),
        "expected custom separator '===', got:\n{output}"
    );
    assert!(
        !output.contains("--\n"),
        "default separator should not appear when custom one is set"
    );
}
