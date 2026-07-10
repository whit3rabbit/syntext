use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use clap::Parser;

use crate::index::Index;
use crate::{Config, SearchOptions};

use super::{
    commands::AgentCommand, manage::cmd_index, manage::cmd_status, manage::cmd_update,
    overlaps_sensitive_prefix, search::cmd_search, search::SearchArgs, Cli, ManageCommand,
};

/// Build a temporary index from a list of (relative_path, content) pairs.
/// Returns (repo_dir, index_dir, config) — caller must keep all three alive.
fn build_index_for_files(files: &[(&str, &str)]) -> (tempfile::TempDir, tempfile::TempDir, Config) {
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
    assert!(cli.line_number > 0, "last line-number flag should win");
    assert!(
        cli.no_line_number == 0,
        "overridden no-line-number should be cleared"
    );
    assert!(cli.with_filename, "last filename flag should win");
    assert!(!cli.no_filename, "overridden no-filename should be cleared");
}

#[test]
fn duplicate_line_number_flag_is_idempotent() {
    let cli = Cli::try_parse_from(["st", "-n", "pattern", "src", "-n"]).expect("parse failed");
    assert!(cli.line_number > 0);
    assert_eq!(cli.pattern.as_deref(), Some("pattern"));
    assert_eq!(cli.paths, [PathBuf::from("src")]);
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
fn agent_claude_commands_parse_scope_flags() {
    let cli = Cli::try_parse_from(["st", "agent", "install", "claude", "--global"])
        .expect("parse failed");
    match cli.command {
        Some(ManageCommand::Agent {
            command: AgentCommand::Install { agent, scope },
        }) => {
            assert_eq!(agent, "claude");
            assert!(scope.global);
            assert!(!scope.project);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let cli =
        Cli::try_parse_from(["st", "agent", "show", "claude", "--project"]).expect("parse failed");
    assert!(matches!(
        cli.command,
        Some(ManageCommand::Agent {
            command: AgentCommand::Show { .. }
        })
    ));
}

#[test]
fn agent_commands_parse_supported_agent_scope_matrix() {
    let cases = [
        ("claude", "--global"),
        ("claude", "--project"),
        ("cursor", "--global"),
        ("copilot", "--project"),
        ("gemini", "--global"),
        ("opencode", "--global"),
        ("openclaw", "--global"),
        ("codex", "--global"),
        ("codex", "--project"),
        ("cline", "--project"),
        ("windsurf", "--project"),
        ("kilocode", "--project"),
        ("antigravity", "--project"),
    ];

    for (agent, scope_flag) in cases {
        let cli =
            Cli::try_parse_from(["st", "agent", "install", agent, scope_flag]).expect("parse");
        match cli.command {
            Some(ManageCommand::Agent {
                command:
                    AgentCommand::Install {
                        agent: parsed,
                        scope,
                    },
            }) => {
                assert_eq!(parsed, agent);
                assert_eq!(scope.global, scope_flag == "--global");
                assert_eq!(scope.project, scope_flag == "--project");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

#[test]
fn init_commands_parse_rtk_style_flags() {
    let cli = Cli::try_parse_from(["st", "init"]).expect("parse failed");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert!(!args.scope.global);
    assert!(!args.scope.project);
    assert_eq!(super::init::resolve_init_agent(&args).unwrap(), "claude");
    assert_eq!(
        super::init::resolve_init_scope(&args, "claude"),
        crate::hook::vendors::InstallScope::Project
    );

    let cli = Cli::try_parse_from(["st", "init", "-g"]).expect("parse failed");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert!(args.scope.global);
    assert_eq!(
        super::init::resolve_init_scope(&args, "claude"),
        crate::hook::vendors::InstallScope::Global
    );

    let cli = Cli::try_parse_from(["st", "init", "-g", "--agent", "cursor"]).expect("parse failed");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert_eq!(super::init::resolve_init_agent(&args).unwrap(), "cursor");
    assert_eq!(
        super::init::resolve_init_scope(&args, "cursor"),
        crate::hook::vendors::InstallScope::Global
    );

    let cli = Cli::try_parse_from(["st", "init", "-g", "--copilot"]).expect("parse failed");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert_eq!(super::init::resolve_init_agent(&args).unwrap(), "copilot");
    assert_eq!(
        super::init::resolve_init_scope(&args, "copilot"),
        crate::hook::vendors::InstallScope::Project
    );
}

#[test]
fn init_rejects_ambiguous_agent_selection() {
    let cli = Cli::try_parse_from(["st", "init", "--agent", "cursor", "--gemini"]).expect("parse");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert!(super::init::resolve_init_agent(&args).is_err());

    let cli = Cli::try_parse_from(["st", "init", "--cursor", "--gemini"]).expect("parse");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert!(super::init::resolve_init_agent(&args).is_err());
}

#[test]
fn init_fsmonitor_flag_parses_and_defaults_to_false() {
    let cli = Cli::try_parse_from(["st", "init"]).expect("parse failed");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert!(!args.fsmonitor, "--fsmonitor must default to false");

    let cli = Cli::try_parse_from(["st", "init", "--fsmonitor"]).expect("parse failed");
    let Some(ManageCommand::Init(args)) = cli.command else {
        panic!("expected init command");
    };
    assert!(args.fsmonitor);
}

#[test]
fn hidden_hook_commands_parse() {
    let cli = Cli::try_parse_from(["st", "__hook", "claude"]).expect("parse failed");
    assert!(matches!(
        cli.command,
        Some(ManageCommand::Hook { ref target }) if target == "claude"
    ));

    let cli = Cli::try_parse_from(["st", "__rewrite", "rg foo src/"]).expect("parse failed");
    assert!(matches!(
        cli.command,
        Some(ManageCommand::Rewrite { ref command, cwd: None }) if command == "rg foo src/"
    ));

    let cli = Cli::try_parse_from(["st", "__rewrite", "--cwd", "/tmp/repo", "rg foo src/"])
        .expect("parse failed");
    assert!(matches!(
        cli.command,
        Some(ManageCommand::Rewrite { ref command, ref cwd })
            if command == "rg foo src/" && cwd.as_deref() == Some(std::path::Path::new("/tmp/repo"))
    ));
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
    super::render::render_with_context_to(
        &config,
        &matches,
        &std::collections::HashMap::new(),
        &args,
        &mut buf,
    )
    .unwrap();
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
fn cmd_status_does_not_error_without_git_repo() {
    // No `git init`: the index root is a plain directory, so all three
    // freshness-detection git commands exit non-zero (non-git directory).
    // `detect_files_behind` must treat that as "no data" (files_behind
    // reported, not an error), and cmd_status must still exit 0.
    let (_repo, _idx, config) = build_index_for_files(&[("hello.rs", "fn hello() {}\n")]);
    assert_eq!(
        cmd_status(config.clone(), true),
        0,
        "cmd_status --json must not error in a non-git directory"
    );
    assert_eq!(
        cmd_status(config, false),
        0,
        "cmd_status text output must not error in a non-git directory"
    );
}

/// Build a temporary index inside a git repo from (path, content) pairs.
/// Initialises git, configures identity, creates an initial commit so
/// `git diff HEAD` has a baseline. Returns (repo_dir, index_dir, Config).
fn build_index_for_files_git(
    files: &[(&str, &str)],
) -> (tempfile::TempDir, tempfile::TempDir, Config) {
    let repo = tempfile::TempDir::new().unwrap();
    let idx = tempfile::TempDir::new().unwrap();

    // Initialize git repo and configure identity.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    for (name, content) in files {
        let p = repo.path().join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }
    git(&["add", "-A"]);
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["commit", "-m", "initial", "--no-gpg-sign"])
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .unwrap();

    let config = Config {
        index_dir: idx.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    assert_eq!(cmd_index(config.clone(), false, false, true), 0);
    (repo, idx, config)
}

#[test]
fn auto_update_finds_new_file_after_index_built() {
    let (repo, idx, _base_config) = build_index_for_files_git(&[("a.rs", "fn hello() {}\n")]);

    // Write a new file after the index was built.
    fs::write(repo.path().join("b.rs"), "fn secret_token() {}\n").unwrap();

    // Search with auto-update enabled: should find the new file.
    let config = Config {
        index_dir: idx.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        auto_update: true,
        ..Config::default()
    };
    let args = SearchArgs {
        pattern: "secret_token".to_string(),
        quiet: true,
        ..Default::default()
    };
    let code = cmd_search(config.clone(), &args);
    assert_eq!(
        code, 0,
        "auto-update should find new file content; got exit code {code}"
    );

    // Write a second new file; search with auto-update disabled so it
    // should NOT find the content.
    fs::write(repo.path().join("c.rs"), "fn another_token() {}\n").unwrap();
    let mut config_no_update = config;
    config_no_update.auto_update = false;
    let args2 = SearchArgs {
        pattern: "another_token".to_string(),
        quiet: true,
        ..Default::default()
    };
    let code = cmd_search(config_no_update, &args2);
    assert_ne!(
        code, 0,
        "with auto_update disabled, new file c.rs should not be found; got exit code {code}"
    );
}

#[test]
fn auto_update_over_max_files_does_not_crash_search() {
    let (repo, idx, _base_config) = build_index_for_files_git(&[("a.rs", "fn hello() {}\n")]);

    // Write more files than auto_update_max_files.
    for i in 0..10 {
        fs::write(
            repo.path().join(format!("mod_{i}.rs")),
            format!("fn mod_{i}() {{ /* unique_marker */ }}\n"),
        )
        .unwrap();
    }

    // Set a very low max_files so the auto-update bails out.
    let config = Config {
        index_dir: idx.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        auto_update: true,
        auto_update_max_files: 3,
        ..Config::default()
    };

    // Search for a pattern that exists in the indexed file (a.rs).
    // The auto-update bails with TooManyFiles, but search still works
    // against the stale index and never returns exit code 2 (error).
    let args = SearchArgs {
        pattern: "hello".to_string(),
        quiet: true,
        ..Default::default()
    };
    let code = cmd_search(config, &args);
    assert!(
        code == 0 || code == 1,
        "search should complete successfully even when auto-update bails; got exit code {code}"
    );
}

#[test]
fn max_file_size_is_clamped_to_ceiling() {
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
        "value above 512 MiB must be clamped to MAX_FILE_SIZE_CEILING"
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
    assert_eq!(
        super::render::build_effective_pattern(&args),
        ("foo".to_string(), Some("^(?:foo)$".to_string()))
    );
}

#[test]
fn word_regexp_groups_alternation_so_all_alternatives_anchor() {
    // Regression for `-w` with multiple `-e`: the joined pattern
    // `(?:foo)|(?:bar)` must be grouped inside the word boundaries, otherwise
    // `\b` binds only the first/last alternative (`\b(?:foo)|(?:bar)\b`) and
    // matches substrings like `foobar` / `xbar`.
    let args = super::search::SearchArgs {
        pattern: "(?:foo)|(?:bar)".to_string(),
        word_regexp: true,
        ..super::search::SearchArgs::default()
    };
    assert_eq!(
        super::render::build_effective_pattern(&args),
        (
            "(?:foo)|(?:bar)".to_string(),
            Some(r"\b(?:(?:foo)|(?:bar))\b".to_string())
        )
    );
}

#[test]
fn word_regexp_wraps_single_pattern() {
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        word_regexp: true,
        ..super::search::SearchArgs::default()
    };
    assert_eq!(
        super::render::build_effective_pattern(&args),
        ("foo".to_string(), Some(r"\b(?:foo)\b".to_string()))
    );
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
fn fixed_strings_multiple_regexp_escapes_each_alternative() {
    // Regression for: `st -F -e 'a.b' -e 'c|d'` must search for the literal
    // "a.b" OR "c|d", NOT the literal string "(?:a.b)|(?:c|d)".
    //
    // The combination+escaping happens in cli::run(); the contract it must
    // uphold is: after combination, `pattern` is a valid regex matching the
    // intended literals and `fixed_strings` is cleared so build_effective_pattern
    // does not re-escape it. We exercise that contract via run_search with the
    // exact SearchArgs run() would produce.
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();

    // "xay" matches literal "a.b"? No — under -F, "a.b" is literal, so only the
    // exact substring "a.b" matches. The file with the literal "a.b" must match.
    fs::write(repo.path().join("dot.rs"), "token = a.b;\n").unwrap();
    fs::write(repo.path().join("pipe.rs"), "flag = c|d;\n").unwrap();
    fs::write(repo.path().join("none.rs"), "no match here\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    assert_eq!(cmd_index(config.clone(), false, false, true), 0);

    let index = Index::open(config.clone()).unwrap();
    // Mirrors run()'s output for `st -F -e 'a.b' -e 'c|d'`: each alternative
    // escaped, joined, and fixed_strings cleared.
    let args = super::search::SearchArgs {
        pattern: "(?:a\\.b)|(?:c\\|d)".to_string(),
        fixed_strings: false,
        ..super::search::SearchArgs::default()
    };

    let results = super::search::run_search(&index, &config, &args).unwrap();
    let matched: Vec<_> = results
        .iter()
        .map(|m| m.path.to_string_lossy().into_owned())
        .collect();
    assert!(
        matched.iter().any(|p| p == "dot.rs"),
        "literal 'a.b' must match dot.rs: {matched:?}"
    );
    assert!(
        matched.iter().any(|p| p == "pipe.rs"),
        "literal 'c|d' must match pipe.rs: {matched:?}"
    );
    assert!(
        !matched.iter().any(|p| p == "none.rs"),
        "non-matching file must be absent: {matched:?}"
    );
    drop(index);
}

#[test]
fn smart_case_short_flag_is_capital_s() {
    let cli = Cli::try_parse_from(["st", "-S", "pat"]).unwrap();
    assert!(cli.smart_case);
}

#[test]
fn color_and_colors_flags_are_accepted() {
    // --color accepts the four rg WHEN values; the field holds the parsed string.
    let cli = Cli::try_parse_from(["st", "--color", "never", "pat"]).unwrap();
    assert_eq!(cli.color.as_deref(), Some("never"));

    let cli = Cli::try_parse_from(["st", "--color", "auto", "pat"]).unwrap();
    assert_eq!(cli.color.as_deref(), Some("auto"));

    // An unknown WHEN is rejected by the value_parser.
    let err = match Cli::try_parse_from(["st", "--color", "bogus", "pat"]) {
        Ok(_) => panic!("expected --color bogus to be rejected"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);

    // --colors SPEC is accepted (parsed but not yet honored).
    let cli = Cli::try_parse_from(["st", "--colors", "match:fg:red", "pat"]).unwrap();
    assert_eq!(cli.colors, ["match:fg:red"]);
}

#[test]
fn resolve_color_decision() {
    use super::render::{resolve_color, ColorWhen};
    let parse = |s: Option<&str>| ColorWhen::parse(s);
    // --pretty forces color on unless an explicit --color=never is given.
    assert!(resolve_color(parse(None), true));
    assert!(!resolve_color(parse(Some("never")), true));
    // always/ansi force on; never forces off regardless of tty.
    assert!(resolve_color(parse(Some("always")), false));
    assert!(resolve_color(parse(Some("ansi")), false));
    assert!(!resolve_color(parse(Some("never")), false));
    // auto (or no flag) without --pretty follows the TTY.
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    assert_eq!(resolve_color(parse(Some("auto")), false), tty);
    assert_eq!(resolve_color(parse(None), false), tty);
}

#[test]
fn color_off_emits_no_ansi_in_flat_output() {
    let (_repo, _idx, config) = build_index_for_files(&[("src/lib.rs", "fn needle() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "needle".to_string(),
        color: false,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    assert!(
        !buf.contains(&0x1b),
        "no ANSI escapes expected with color off, got: {buf:?}"
    );
    drop(index);
}

#[test]
fn color_on_wraps_match_path_and_line_in_flat_output() {
    let (_repo, _idx, config) = build_index_for_files(&[("src/lib.rs", "fn needle() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "needle".to_string(),
        color: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    // Match text is bold red, path is magenta, line number is green.
    assert!(
        out.contains("\x1b[1;31mneedle\x1b[0m"),
        "expected highlighted match, got: {out:?}"
    );
    assert!(
        out.contains("\x1b[35msrc/lib.rs\x1b[0m"),
        "expected highlighted path, got: {out:?}"
    );
    assert!(
        out.contains("\x1b[32m1\x1b[0m"),
        "expected highlighted line number, got: {out:?}"
    );
    drop(index);
}

#[test]
fn color_in_heading_wraps_header_path_line_and_match() {
    let (_repo, _idx, config) = build_index_for_files(&[("src/lib.rs", "fn needle() {}\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "needle".to_string(),
        heading: true,
        color: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    assert!(!results.is_empty());
    let mut buf = Vec::<u8>::new();
    super::render::render_heading_to(&results, &args, &mut buf).unwrap();
    let out = String::from_utf8(buf).unwrap();
    // Heading colors the filename header, the per-line number, and the match.
    assert!(
        out.contains("\x1b[35msrc/lib.rs\x1b[0m"),
        "header path: {out:?}"
    );
    assert!(out.contains("\x1b[1;31mneedle\x1b[0m"), "match: {out:?}");
    assert!(out.contains("\x1b[32m1\x1b[0m"), "line number: {out:?}");
    drop(index);
}

#[test]
fn noops_ignore_and_discovery_flags_are_accepted() {
    let cli = Cli::try_parse_from([
        "st",
        "--no-ignore",
        "--no-ignore-vcs",
        "--hidden",
        "--follow",
        "pat",
    ])
    .unwrap();
    assert!(
        cli.compat.no_ignore && cli.compat.no_ignore_vcs && cli.compat.hidden && cli.compat.follow
    );
}

#[test]
fn noops_unrestricted_counts_repetitions() {
    let cli = Cli::try_parse_from(["st", "-uuu", "pat"]).unwrap();
    assert_eq!(cli.compat.unrestricted, 3);
}

#[test]
fn noops_sort_and_sortr_are_accepted() {
    let cli = Cli::try_parse_from(["st", "--sort", "path", "pat"]).unwrap();
    assert_eq!(cli.compat.sort.as_deref(), Some("path"));

    let cli = Cli::try_parse_from(["st", "--sortr", "modified", "pat"]).unwrap();
    assert_eq!(cli.compat.sortr.as_deref(), Some("modified"));
}

#[test]
fn noops_binary_text_encoding_flags_are_accepted() {
    let cli = Cli::try_parse_from([
        "st",
        "-a",
        "--binary",
        "-E",
        "utf-8",
        "--crlf",
        "--null-data",
        "pat",
    ])
    .unwrap();
    assert!(cli.compat.text && cli.compat.binary && cli.compat.crlf && cli.compat.null_data);
    assert_eq!(cli.compat.encoding.as_deref(), Some("utf-8"));
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
    assert!(cli.compat.multiline && cli.compat.multiline_dotall);
    assert_eq!(cli.compat.engine.as_deref(), Some("auto"));
    assert_eq!(cli.compat.dfa_size_limit.as_deref(), Some("128M"));
    assert_eq!(cli.compat.regex_size_limit.as_deref(), Some("64M"));
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
    assert_eq!(cli.compat.pattern_file, Some(PathBuf::from("/tmp/p.txt")));
    assert_eq!(cli.compat.type_add, ["mine:*.x"]);
    assert_eq!(cli.compat.type_clear, ["go"]);
    assert_eq!(cli.compat.iglob.as_deref(), Some("*.txt"));
}

#[test]
fn repeated_glob_and_type_filters_are_accepted() {
    let cli = Cli::try_parse_from([
        "st",
        "-g",
        "*.rs",
        "--glob",
        "!tests/**",
        "-t",
        "rs",
        "-t",
        "md",
        "-T",
        "log",
        "-T",
        "tmp",
        "needle",
    ])
    .expect("parse failed");

    assert_eq!(cli.glob, ["*.rs", "!tests/**"]);
    assert_eq!(cli.file_type, ["rs", "md"]);
    assert_eq!(cli.type_not, ["log", "tmp"]);
}

#[test]
fn regexp_allows_hyphen_leading_pattern() {
    let cli = Cli::try_parse_from(["st", "-F", "-e", "--global", "src"]).expect("parse failed");
    assert_eq!(cli.regexp, ["--global"]);
    assert_eq!(cli.pattern.as_deref(), Some("src"));
}

#[test]
fn repeated_regexp_values_are_appended() {
    let cli = Cli::try_parse_from(["st", "-e", "foo", "-e", "bar", "src"]).expect("parse failed");
    assert_eq!(cli.regexp, ["foo", "bar"]);
    assert_eq!(cli.pattern.as_deref(), Some("src"));
}

#[test]
fn include_and_exclude_aliases_feed_glob_set() {
    let cli = Cli::try_parse_from([
        "st",
        "--include",
        "*.rs",
        "--exclude",
        "*tests.rs",
        "needle",
    ])
    .expect("parse failed");

    assert_eq!(cli.include, ["*.rs"]);
    assert_eq!(cli.exclude, ["*tests.rs"]);
    assert_eq!(cli.combined_globs(), ["*.rs", "!*tests.rs"]);
}

#[test]
fn index_alias_matches_index_dir() {
    let cli =
        Cli::try_parse_from(["st", "--index", "/tmp/st-index", "needle"]).expect("parse failed");
    assert_eq!(cli.index_dir, Some(PathBuf::from("/tmp/st-index")));
    assert_eq!(cli.pattern.as_deref(), Some("needle"));
}

#[test]
fn noops_threads_mmap_performance_flags_are_accepted() {
    let cli = Cli::try_parse_from(["st", "-j", "4", "--mmap", "pat"]).unwrap();
    assert_eq!(cli.compat.threads, Some(4));
    assert!(cli.compat.mmap);

    let cli = Cli::try_parse_from(["st", "--no-mmap", "pat"]).unwrap();
    assert!(cli.compat.no_mmap);
}

#[test]
fn noops_preprocessing_and_zip_are_accepted() {
    let cli =
        Cli::try_parse_from(["st", "--pre", "decomp", "--pre-glob", "*.gz", "-z", "pat"]).unwrap();
    assert_eq!(cli.compat.pre.as_deref(), Some("decomp"));
    assert_eq!(cli.compat.pre_glob.as_deref(), Some("*.gz"));
    assert!(cli.compat.search_zip);
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
    assert!(
        cli.compat.trace
            && cli.compat.no_messages
            && cli.compat.no_config
            && cli.compat.one_file_system
    );
}

#[test]
fn noops_max_filesize_is_accepted() {
    let cli = Cli::try_parse_from(["st", "--max-filesize", "1M", "pat"]).unwrap();
    assert_eq!(cli.compat.max_filesize.as_deref(), Some("1M"));
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
    assert!(cli.compat.debug);
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
fn multi_glob_filters_or_positive_and_apply_excludes() {
    let (_repo, _idx, config) = build_index_for_files(&[
        ("src/lib.rs", "needle\n"),
        ("docs/readme.md", "needle\n"),
        ("tests/lib_tests.rs", "needle\n"),
        ("src/tool.py", "needle\n"),
    ]);
    let index = Index::open(config.clone()).unwrap();

    let args = super::search::SearchArgs {
        pattern: "needle".to_string(),
        globs: vec![
            "*.rs".to_string(),
            "*.md".to_string(),
            "!*tests.rs".to_string(),
        ],
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    let paths: Vec<_> = results.iter().map(|m| m.path.as_path()).collect();

    assert!(paths.contains(&std::path::Path::new("src/lib.rs")));
    assert!(paths.contains(&std::path::Path::new("docs/readme.md")));
    assert!(!paths.contains(&std::path::Path::new("tests/lib_tests.rs")));
    assert!(!paths.contains(&std::path::Path::new("src/tool.py")));

    drop(index);
}

#[test]
fn multi_type_filters_or_includes_and_apply_excludes() {
    let (_repo, _idx, config) = build_index_for_files(&[
        ("src/lib.rs", "needle\n"),
        ("docs/readme.md", "needle\n"),
        ("logs/debug.log", "needle\n"),
        ("src/tool.py", "needle\n"),
    ]);
    let index = Index::open(config.clone()).unwrap();

    let args = super::search::SearchArgs {
        pattern: "needle".to_string(),
        file_types: vec!["rs".to_string(), "md".to_string(), "log".to_string()],
        type_nots: vec!["log".to_string()],
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    let paths: Vec<_> = results.iter().map(|m| m.path.as_path()).collect();

    assert!(paths.contains(&std::path::Path::new("src/lib.rs")));
    assert!(paths.contains(&std::path::Path::new("docs/readme.md")));
    assert!(!paths.contains(&std::path::Path::new("logs/debug.log")));
    assert!(!paths.contains(&std::path::Path::new("src/tool.py")));

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
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "fn foo() {}\nfn bar() {}\n")]);
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
    assert!(
        !output.contains("bar"),
        "bar should not appear in results for 'foo'"
    );
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
fn byte_offset_prints_line_start_not_match_start() {
    // Regression: --byte-offset printed the match offset; rg prints the
    // line-start offset. "xxfoo": match "foo" is at byte 2, the line starts at
    // byte 0, so rg emits "0:" (syntext previously emitted "2:").
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "xxfoo\n")]);
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
        output.starts_with("0:"),
        "expected line-start byte offset 0, got: {output:?}"
    );
    drop(index);
}

#[test]
fn word_regexp_returns_only_whole_word_matches() {
    // Regression: -w on a literal routed verification through memchr on the
    // wrapped regex string `\b(?:foo)\b` (matching nothing). It must verify
    // with the regex and return only whole-word matches, like rg.
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "foo\nfoobar\nbaz foo qux\n")]);
    let index = Index::open(config.clone()).unwrap();
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        word_regexp: true,
        no_filename: true,
        no_line_number: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    let lines: Vec<&str> = results
        .iter()
        .map(|m| std::str::from_utf8(&m.line_content).unwrap().trim_end())
        .collect();
    // `foo` (line 1) and `baz foo qux` (line 3) are word-bounded; `foobar`
    // (line 2) is not. Note `_` is a regex word char, so non-word delimiters
    // (letters/spaces) are used to match rg's \b semantics.
    assert!(
        lines.contains(&"foo"),
        "must match whole-word `foo`, got {lines:?}"
    );
    assert!(
        lines.contains(&"baz foo qux"),
        "must match `foo` between spaces, got {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("foobar")),
        "must NOT match `foobar` (foo not word-bounded), got {lines:?}"
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
    assert!(
        output.contains("[Omitted long matching line]"),
        "placeholder should be present"
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
    super::render::render_with_context_to(
        &config,
        &matches,
        &std::collections::HashMap::new(),
        &args,
        &mut buf,
    )
    .unwrap();
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

// --- overlaps_sensitive_prefix tests (platform-independent) ---

#[test]
fn sensitive_prefix_rejects_exact_match_unix() {
    let prefixes = &["/etc", "/usr", "/bin"];
    assert_eq!(
        overlaps_sensitive_prefix("/etc", prefixes, '/'),
        Some("/etc")
    );
}

#[test]
fn sensitive_prefix_rejects_subpath_unix() {
    let prefixes = &["/etc", "/usr"];
    assert_eq!(
        overlaps_sensitive_prefix("/etc/syntext", prefixes, '/'),
        Some("/etc"),
    );
}

#[test]
fn sensitive_prefix_accepts_safe_path_unix() {
    let prefixes = &["/etc", "/usr"];
    assert_eq!(
        overlaps_sensitive_prefix("/home/user/index", prefixes, '/'),
        None
    );
}

#[test]
fn sensitive_prefix_no_false_positive_on_prefix_substring() {
    // "/etcetera" should NOT match "/etc" because there is no separator after "/etc".
    let prefixes = &["/etc"];
    assert_eq!(overlaps_sensitive_prefix("/etcetera", prefixes, '/'), None);
}

#[test]
fn sensitive_prefix_rejects_exact_match_windows() {
    let prefixes = &["c:\\windows", "c:\\program files"];
    assert_eq!(
        overlaps_sensitive_prefix("c:\\windows", prefixes, '\\'),
        Some("c:\\windows"),
    );
}

#[test]
fn sensitive_prefix_rejects_subpath_windows() {
    let prefixes = &["c:\\windows"];
    assert_eq!(
        overlaps_sensitive_prefix("c:\\windows\\system32\\foo", prefixes, '\\'),
        Some("c:\\windows"),
    );
}

#[test]
fn sensitive_prefix_accepts_safe_path_windows() {
    let prefixes = &["c:\\windows", "c:\\program files"];
    assert_eq!(
        overlaps_sensitive_prefix("d:\\projects\\index", prefixes, '\\'),
        None,
    );
}

#[test]
fn cli_parses_verify_subcommand() {
    let cli = Cli::try_parse_from(["st", "verify"]).expect("parse failed");
    assert!(matches!(cli.command, Some(ManageCommand::Verify)));
}

#[test]
fn cmd_search_missing_index_exits_2() {
    let repo = tempfile::TempDir::new().unwrap();
    let missing = tempfile::TempDir::new().unwrap();
    let config = Config {
        repo_root: repo.path().to_path_buf(),
        index_dir: missing.path().join("no-such-index"),
        ..Config::default()
    };
    let args = super::search::SearchArgs {
        pattern: "foo".to_string(),
        ..super::search::SearchArgs::default()
    };
    assert_eq!(super::search::cmd_search(config, &args), 2);
}

#[test]
#[cfg(feature = "symbols")]
fn cmd_search_rejects_incompatible_symbol_flags() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = tempfile::TempDir::new().unwrap();
    let config = Config {
        repo_root: repo.path().to_path_buf(),
        index_dir: index_dir.path().to_path_buf(),
        ..Config::default()
    };

    // Create a dummy index manifest so Index::open doesn't fail with IndexNotFound
    let manifest = crate::index::manifest::Manifest::new(vec![], 0);
    manifest.save(index_dir.path()).unwrap();

    // Symbol queries now come from --sym, not a pattern prefix.
    // Try count flag with a symbol lookup.
    let args = super::search::SearchArgs {
        sym: Some("foo".to_string()),
        count: true,
        ..super::search::SearchArgs::default()
    };
    #[cfg(feature = "symbols")]
    assert_eq!(super::search::cmd_search(config.clone(), &args), 2);

    // Try json flag with a symbol lookup.
    let args = super::search::SearchArgs {
        sym: Some("foo".to_string()),
        json: true,
        ..super::search::SearchArgs::default()
    };
    #[cfg(feature = "symbols")]
    assert_eq!(super::search::cmd_search(config.clone(), &args), 2);

    // Try only-matching flag with a symbol lookup.
    let args = super::search::SearchArgs {
        sym: Some("foo".to_string()),
        only_matching: true,
        ..super::search::SearchArgs::default()
    };
    #[cfg(feature = "symbols")]
    assert_eq!(super::search::cmd_search(config.clone(), &args), 2);
}

#[test]
fn trim_column_adjustment() {
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "   match_here\n")]);
    let index = Index::open(config.clone()).unwrap();

    // 1. flat renderer --column
    let args = super::search::SearchArgs {
        pattern: "match".to_string(),
        column: true,
        trim: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let results = super::search::run_search(&index, &config, &args).unwrap();
    let mut buf = Vec::new();
    super::render::render_flat_to(&results, &args, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    // Original column of "match" in "   match_here" is 4 (1-based).
    // Trimmed 3 spaces, so adjusted column should be 4 - 3 = 1.
    assert_eq!(output, "1:1:match_here\n");

    // 2. vimgrep renderer
    let args_vim = super::search::SearchArgs {
        pattern: "match".to_string(),
        vimgrep: true,
        trim: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let mut buf_vim = Vec::new();
    super::render::render_vimgrep_to(&results, &args_vim, &mut buf_vim).unwrap();
    let output_vim = String::from_utf8(buf_vim).unwrap();
    assert!(
        output_vim.contains("a.rs:1:1:match_here\n"),
        "vimgrep output was: {:?}",
        output_vim
    );

    drop(index);
}

#[test]
fn max_columns_placeholders() {
    let (_repo, _idx, config) =
        build_index_for_files(&[("a.rs", "long_line_with_match_and_another_match\n")]);
    let index = Index::open(config.clone()).unwrap();
    let results = super::search::run_search(
        &index,
        &config,
        &super::search::SearchArgs {
            pattern: "match".to_string(),
            ..super::search::SearchArgs::default()
        },
    )
    .unwrap();

    // 1. Column mode (should show count of matches on line)
    let args_col = super::search::SearchArgs {
        pattern: "match".to_string(),
        max_columns: Some(10),
        column: true,
        no_filename: true,
        ..super::search::SearchArgs::default()
    };
    let mut buf_col = Vec::new();
    super::render::render_flat_to(&results, &args_col, &mut buf_col).unwrap();
    let output_col = String::from_utf8(buf_col).unwrap();
    assert_eq!(output_col, "1:16:[Omitted long line with 2 matches]\n");

    // 2. Vimgrep mode (should show count of matches and print for each hit)
    let args_vim = super::search::SearchArgs {
        pattern: "match".to_string(),
        max_columns: Some(10),
        vimgrep: true,
        ..super::search::SearchArgs::default()
    };
    let mut buf_vim = Vec::new();
    super::render::render_vimgrep_to(&results, &args_vim, &mut buf_vim).unwrap();
    let output_vim = String::from_utf8(buf_vim).unwrap();
    assert!(output_vim.contains("a.rs:1:16:[Omitted long line with 2 matches]\n"));
    assert!(output_vim.contains("a.rs:1:34:[Omitted long line with 2 matches]\n"));

    drop(index);
}

#[test]
fn null_byte_path_separators() {
    let (_repo, _idx, config) = build_index_for_files(&[("a.rs", "match\n")]);
    let index = Index::open(config.clone()).unwrap();
    let results = super::search::run_search(
        &index,
        &config,
        &super::search::SearchArgs {
            pattern: "match".to_string(),
            ..super::search::SearchArgs::default()
        },
    )
    .unwrap();

    // 1. Flat renderer with --null
    let args_flat = super::search::SearchArgs {
        pattern: "match".to_string(),
        null: true,
        ..super::search::SearchArgs::default()
    };
    let mut buf_flat = Vec::new();
    super::render::render_flat_to(&results, &args_flat, &mut buf_flat).unwrap();
    let expected_flat = b"a.rs\x001:match\n";
    assert_eq!(buf_flat, expected_flat);

    // 2. Flat --column with --null
    let args_col = super::search::SearchArgs {
        pattern: "match".to_string(),
        column: true,
        null: true,
        ..super::search::SearchArgs::default()
    };
    let mut buf_col = Vec::new();
    super::render::render_flat_to(&results, &args_col, &mut buf_col).unwrap();
    let expected_col = b"a.rs\x001:1:match\n";
    assert_eq!(buf_col, expected_col);

    // 3. Vimgrep with --null
    let args_vim = super::search::SearchArgs {
        pattern: "match".to_string(),
        vimgrep: true,
        null: true,
        ..super::search::SearchArgs::default()
    };
    let mut buf_vim = Vec::new();
    super::render::render_vimgrep_to(&results, &args_vim, &mut buf_vim).unwrap();
    let expected_vim = b"a.rs\x001:1:match\n";
    assert_eq!(buf_vim, expected_vim);

    drop(index);
}
