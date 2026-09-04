use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn st() -> Command {
    Command::new(env!("CARGO_BIN_EXE_st"))
}

fn run(args: &[&str]) -> Output {
    st().args(args).output().expect("run st")
}

fn run_repo(repo: &Path, index: &Path, args: &[&str]) -> Output {
    st().arg("--repo-root")
        .arg(repo)
        .arg("--index-dir")
        .arg(index)
        .args(args)
        .output()
        .expect("run st with repo")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stdout_lines_with_newlines(output: &Output) -> Vec<&[u8]> {
    output
        .stdout
        .split_inclusive(|&byte| byte == b'\n')
        .collect()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_text(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_bytes(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn build_index(repo: &Path, index: &Path) {
    let output = run_repo(repo, index, &["index", "--quiet"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "index failed\nstdout:\n{}\nstderr:\n{}",
        stdout_text(&output),
        stderr_text(&output)
    );
}

fn base64_encode(bytes: &[u8]) -> String {
    syntext::__internal::encode(bytes)
}

fn fix_path(text: String) -> String {
    if cfg!(windows) {
        text.replace("\\", "/")
    } else {
        text
    }
}

#[test]
fn missing_pattern_exits_with_usage_error() {
    let output = run(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_text(&output).contains("pattern is required"));
}

#[test]
fn invalid_flag_exits_with_clap_error() {
    let output = run(&["--wat"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_text(&output).contains("unexpected argument"));
}

#[test]
fn search_with_openat2_disabled_still_finds_matches() {
    // Forcing SYNTEXT_NO_OPENAT2 exercises the portable (legacy) open path in
    // io_util::open_beneath even on an openat2-capable kernel, so on Linux CI a
    // normal `cargo test` run covers BOTH branches (default run = openat2 fast
    // path, this test = forced legacy). Results must be identical either way.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/a.rs"),
        "fn openat2_probe_marker() {}\n",
    );
    build_index(repo.path(), index.path());

    let output = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index.path())
        .env("SYNTEXT_NO_OPENAT2", "1")
        .args(["-l", "openat2_probe_marker"])
        .output()
        .expect("run st");
    assert_eq!(
        output.status.code(),
        Some(0),
        "forced-legacy search failed: {}",
        stderr_text(&output)
    );
    assert!(
        fix_path(stdout_text(&output)).contains("src/a.rs"),
        "forced-legacy search must still find the match, got: {}",
        stdout_text(&output)
    );
}

#[test]
fn search_exit_codes_follow_cli_contract() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/lib.rs"),
        "fn needle() {}\nfn helper() {}\n",
    );
    build_index(repo.path(), index.path());

    let hit = run_repo(repo.path(), index.path(), &["needle"]);
    assert_eq!(hit.status.code(), Some(0));
    assert!(fix_path(stdout_text(&hit)).contains("needle"));

    let quiet_hit = run_repo(repo.path(), index.path(), &["-q", "needle"]);
    assert_eq!(quiet_hit.status.code(), Some(0));
    assert!(quiet_hit.stdout.is_empty());

    let miss = run_repo(repo.path(), index.path(), &["absent_symbol"]);
    assert_eq!(miss.status.code(), Some(1));

    let quiet_miss = run_repo(repo.path(), index.path(), &["-q", "absent_symbol"]);
    assert_eq!(quiet_miss.status.code(), Some(1));
    assert!(quiet_miss.stdout.is_empty());

    let invalid = run_repo(repo.path(), index.path(), &["("]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(stderr_text(&invalid).contains("invalid"));

    let quiet_invalid = run_repo(repo.path(), index.path(), &["-q", "("]);
    assert_eq!(quiet_invalid.status.code(), Some(2));
}

#[test]
fn status_json_is_machine_readable() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/main.rs"),
        "fn main() { println!(\"x\"); }\n",
    );
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["documents"].as_u64().unwrap() >= 1);
    assert!(value["segments"].as_u64().unwrap() >= 1);
    assert_eq!(value["index_dir"], index.path().display().to_string());
}

#[test]
fn status_reports_files_behind_for_untracked_files() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/main.rs"),
        "fn main() { println!(\"x\"); }\n",
    );

    // Initialize a git repo and commit the initial file so the index has a
    // base commit to compare against.
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), index.path());

    // Create 3 untracked files after the index was built: the index should
    // now be 3 files behind the working tree.
    write_text(&repo.path().join("new_a.rs"), "fn a() {}\n");
    write_text(&repo.path().join("new_b.rs"), "fn b() {}\n");
    write_text(&repo.path().join("new_c.rs"), "fn c() {}\n");

    let json_output = run_repo(repo.path(), index.path(), &["status", "--json"]);
    assert_eq!(json_output.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(
        value["files_behind"].as_u64(),
        Some(3),
        "files_behind should count the 3 new untracked files, got {value}"
    );

    let text_output = run_repo(repo.path(), index.path(), &["status"]);
    assert_eq!(text_output.status.code(), Some(0));
    let text = stdout_text(&text_output);
    assert!(
        text.lines()
            .any(|line| line.starts_with("Behind:") && line.contains('3')),
        "text status output should show a files-behind line with count 3, got:\n{text}"
    );
}

#[test]
fn status_exits_zero_and_reports_files_behind_without_git_repo() {
    // No `git init`: st status must still succeed, reporting files_behind
    // as unknown/0 rather than erroring the command.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/main.rs"),
        "fn main() { println!(\"x\"); }\n",
    );
    build_index(repo.path(), index.path());

    let json_output = run_repo(repo.path(), index.path(), &["status", "--json"]);
    assert_eq!(
        json_output.status.code(),
        Some(0),
        "status --json must exit 0 in a non-git directory"
    );
    let value: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    // A non-git directory makes every git detection command exit non-zero,
    // which is treated as "no changes found" (0), not an error (null would
    // only occur if the git binary itself could not be resolved at all).
    let behind = value["files_behind"].as_u64();
    assert!(
        behind == Some(0) || value["files_behind"].is_null(),
        "files_behind should be unknown/0 without a git repo, got {value}"
    );

    let text_output = run_repo(repo.path(), index.path(), &["status"]);
    assert_eq!(
        text_output.status.code(),
        Some(0),
        "status text output must exit 0 in a non-git directory"
    );
}

#[cfg(unix)]
#[test]
fn status_json_escapes_special_characters_in_index_dir() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_root = tempfile::TempDir::new().unwrap();
    // Windows doesn't allow " in filenames.
    let index = index_root.path().join("index _quoted_ \\ tab\tline\nbreak");
    write_text(
        &repo.path().join("src/main.rs"),
        "fn main() { println!(\"needle\"); }\n",
    );
    build_index(repo.path(), &index);

    let output = run_repo(repo.path(), &index, &["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));

    let stdout = stdout_text(&output);
    assert_eq!(
        stdout.trim_end_matches('\n').lines().count(),
        1,
        "status --json must stay single-line for line-oriented tooling"
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["index_dir"], index.display().to_string());
}

#[test]
fn json_output_emits_begin_match_end_and_summary_messages() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/one.rs"),
        "fn needle() { println!(\"quote: \\\"x\\\"\"); }\n",
    );
    write_text(&repo.path().join("src/two.rs"), "fn needle() {\t42 }\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "needle"]);
    assert_eq!(output.status.code(), Some(0));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();

    let kinds: Vec<_> = messages
        .iter()
        .map(|msg| msg["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.iter().filter(|&&kind| kind == "begin").count(), 2);
    assert_eq!(kinds.iter().filter(|&&kind| kind == "match").count(), 2);
    assert_eq!(kinds.iter().filter(|&&kind| kind == "end").count(), 2);
    assert_eq!(kinds.last().copied(), Some("summary"));

    let matched_paths: Vec<_> = messages
        .iter()
        .filter(|msg| msg["type"] == "match")
        .map(|msg| fix_path(msg["data"]["path"]["text"].as_str().unwrap().to_string()))
        .collect();
    assert!(matched_paths.contains(&"src/one.rs".to_string()));
    assert!(matched_paths.contains(&"src/two.rs".to_string()));
}

#[test]
fn json_output_reports_all_submatches_on_a_matching_line() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/multi.rs"), "needle needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "needle"]);
    assert_eq!(output.status.code(), Some(0));

    let matched = stdout_text(&output)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid NDJSON line"))
        .find(|msg| msg["type"] == "match")
        .expect("match message");

    assert_eq!(matched["data"]["absolute_offset"], 0);
    let submatches = matched["data"]["submatches"]
        .as_array()
        .expect("submatches array");
    assert_eq!(submatches.len(), 2);
    assert_eq!(submatches[0]["start"], 0);
    assert_eq!(submatches[0]["end"], 6);
    assert_eq!(submatches[1]["start"], 7);
    assert_eq!(submatches[1]["end"], 13);
}

#[test]
fn json_output_summary_counts_full_scoped_corpus() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/hit.txt"), "needle\n");
    write_text(&repo.path().join("src/miss.txt"), "miss\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "needle", "src"]);
    assert_eq!(output.status.code(), Some(0));

    let summary = stdout_text(&output)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid NDJSON line"))
        .find(|msg| msg["type"] == "summary")
        .expect("summary message");

    let stats = &summary["data"]["stats"];
    assert_eq!(stats["searches"], 2);
    assert_eq!(stats["searches_with_match"], 1);
    assert_eq!(stats["bytes_searched"], 12);
    assert_eq!(stats["matched_lines"], 1);
    assert_eq!(stats["matches"], 1);
}

#[test]
fn json_output_stats_report_emitted_payload_bytes() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/one.txt"),
        "before\nneedle needle\nafter\n",
    );
    write_text(&repo.path().join("src/two.txt"), "miss\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--json", "-C", "1", "needle", "src"],
    );
    assert_eq!(output.status.code(), Some(0));

    let raw_lines = stdout_lines_with_newlines(&output);
    let messages: Vec<serde_json::Value> = raw_lines
        .iter()
        .map(|line| serde_json::from_slice(line).expect("valid NDJSON line"))
        .collect();

    let expected_bytes: usize = raw_lines
        .iter()
        .zip(messages.iter())
        .filter(|(_, msg)| {
            matches!(
                msg["type"].as_str(),
                Some("begin") | Some("context") | Some("match")
            )
        })
        .map(|(line, _)| line.len())
        .sum();

    let end = messages
        .iter()
        .find(|msg| msg["type"] == "end")
        .expect("end message");
    let summary = messages
        .iter()
        .find(|msg| msg["type"] == "summary")
        .expect("summary message");
    assert_eq!(end["data"]["stats"]["bytes_printed"], expected_bytes);
    assert_eq!(summary["data"]["stats"]["bytes_printed"], expected_bytes);
}

#[test]
fn json_output_on_no_matches_emits_summary_only() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "miss\n");
    write_text(&repo.path().join("src/two.txt"), "also miss\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "needle", "src"]);
    assert_eq!(output.status.code(), Some(1));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["type"], "summary");
    assert_eq!(messages[0]["data"]["stats"]["searches"], 2);
    assert_eq!(messages[0]["data"]["stats"]["searches_with_match"], 0);
    assert_eq!(messages[0]["data"]["stats"]["bytes_searched"], 15);
    assert_eq!(messages[0]["data"]["stats"]["matched_lines"], 0);
    assert_eq!(messages[0]["data"]["stats"]["matches"], 0);
}

#[test]
fn json_output_emits_context_messages_when_requested() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/context.rs"),
        "before\nneedle here\nafter\n",
    );
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "-C", "1", "needle"]);
    assert_eq!(output.status.code(), Some(0));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();

    let kinds: Vec<_> = messages
        .iter()
        .map(|msg| msg["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec!["begin", "context", "match", "context", "end", "summary"]
    );

    let context_messages: Vec<_> = messages
        .iter()
        .filter(|msg| msg["type"] == "context")
        .collect();
    assert_eq!(context_messages.len(), 2);
    assert_eq!(context_messages[0]["data"]["lines"]["text"], "before\n");
    assert_eq!(
        context_messages[0]["data"]["submatches"],
        serde_json::json!([])
    );
    assert_eq!(context_messages[1]["data"]["lines"]["text"], "after\n");

    let matched = messages
        .iter()
        .find(|msg| msg["type"] == "match")
        .expect("match message");
    assert_eq!(matched["data"]["absolute_offset"], 7);
}

#[cfg(unix)]
#[test]
fn json_output_escapes_special_characters_in_paths_and_lines() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    // Windows doesn't allow " in filenames.
    let rel_path = "src/json _quoted_ \\\\ tab\tline\nfile.txt";
    let expected_line = "prefix needle \"quote\" \t slash\\\\ suffix";
    write_text(&repo.path().join(rel_path), &format!("{expected_line}\n"));
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "-F", "needle"]);
    assert_eq!(output.status.code(), Some(0));

    let stdout = stdout_text(&output);
    let messages: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();

    let begin = messages
        .iter()
        .find(|msg| msg["type"] == "begin")
        .expect("begin message");
    assert_eq!(begin["data"]["path"]["text"], rel_path);

    let matched = messages
        .iter()
        .find(|msg| msg["type"] == "match")
        .expect("match message");
    assert_eq!(matched["data"]["path"]["text"], rel_path);
    assert_eq!(
        matched["data"]["lines"]["text"],
        format!("{expected_line}\n")
    );
    assert_eq!(matched["data"]["submatches"][0]["match"]["text"], "needle");

    let end = messages
        .iter()
        .find(|msg| msg["type"] == "end")
        .expect("end message");
    assert_eq!(end["data"]["path"]["text"], rel_path);
}

#[test]
fn files_with_matches_count_heading_and_context_modes_work() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/sample.rs"),
        "line 1\nneedle on line 2\nline 3\nline 4\nline 5\nneedle on line 6\nline 7\n",
    );
    build_index(repo.path(), index.path());

    let files = run_repo(repo.path(), index.path(), &["-l", "needle"]);
    assert_eq!(files.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&files)), "src/sample.rs\n");

    let counts = run_repo(repo.path(), index.path(), &["-c", "needle"]);
    assert_eq!(counts.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&counts)), "src/sample.rs:2\n");

    let heading = run_repo(repo.path(), index.path(), &["--heading", "needle"]);
    assert_eq!(heading.status.code(), Some(0));
    assert!(fix_path(stdout_text(&heading)).starts_with("src/sample.rs\n"));

    let context = run_repo(repo.path(), index.path(), &["-C", "1", "needle"]);
    assert_eq!(context.status.code(), Some(0));
    let text = fix_path(stdout_text(&context));
    assert!(text.contains("src/sample.rs:needle on line 2"));
    assert!(text.contains("src/sample.rs-line 3"));
    assert!(text.contains("src/sample.rs:needle on line 6"));
    assert!(text.contains("--\n"));
}

#[test]
fn heading_with_context_groups_results_by_file() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "before\nneedle\nafter\n");
    write_text(&repo.path().join("src/two.txt"), "x\nneedle\ny\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--heading", "-n", "-C", "1", "needle", "src"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&output)),
        "src/one.txt\n1-before\n2:needle\n3-after\n\nsrc/two.txt\n1-x\n2:needle\n3-y\n"
    );
}

#[test]
fn heading_no_filename_still_separates_groups_with_a_blank_line() {
    // Regression: rg keeps the blank line between file groups under
    // `--heading --no-filename`; only the printed path text is suppressed,
    // the group separator is not. A prior version nested the `writeln!`
    // inside the same `!args.no_filename` check as the path text, dropping
    // the separator entirely for this flag combination.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "needle\n");
    write_text(&repo.path().join("src/two.txt"), "needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--heading", "--no-filename", "-n", "needle", "src"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_text(&output), "1:needle\n\n1:needle\n");
}

#[test]
fn default_filename_and_line_number_heuristics_match_scope() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.rs"), "needle\n");
    write_text(&repo.path().join("src/two.rs"), "needle\n");
    build_index(repo.path(), index.path());

    let single_file = run_repo(repo.path(), index.path(), &["needle", "src/one.rs"]);
    assert_eq!(single_file.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&single_file)), "needle\n");

    let single_file_with_number =
        run_repo(repo.path(), index.path(), &["-n", "needle", "src/one.rs"]);
    assert_eq!(single_file_with_number.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&single_file_with_number)),
        "1:needle\n"
    );

    let single_file_with_name =
        run_repo(repo.path(), index.path(), &["-H", "needle", "src/one.rs"]);
    assert_eq!(single_file_with_name.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&single_file_with_name)),
        "src/one.rs:needle\n"
    );

    let dir_scope = run_repo(repo.path(), index.path(), &["needle", "src"]);
    assert_eq!(dir_scope.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&dir_scope)),
        "src/one.rs:needle\nsrc/two.rs:needle\n"
    );

    let count_single = run_repo(repo.path(), index.path(), &["-c", "needle", "src/one.rs"]);
    assert_eq!(count_single.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&count_single)), "1\n");

    let count_single_named = run_repo(
        repo.path(),
        index.path(),
        &["-c", "-H", "needle", "src/one.rs"],
    );
    assert_eq!(count_single_named.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&count_single_named)), "src/one.rs:1\n");
}

#[test]
fn multiple_path_arguments_are_all_searched() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.rs"), "needle in one\n");
    write_text(&repo.path().join("lib/two.rs"), "needle in two\n");
    write_text(&repo.path().join("tests/three.rs"), "needle in three\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["needle", "src/one.rs", "lib"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&output)),
        "lib/two.rs:needle in two\nsrc/one.rs:needle in one\n"
    );
}

#[test]
fn overlapping_path_scopes_do_not_duplicate_matches() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.rs"), "needle once\n");
    write_text(&repo.path().join("src/two.rs"), "needle twice\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["needle", "src", "src/one.rs"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&output)),
        "src/one.rs:needle once\nsrc/two.rs:needle twice\n"
    );
}

#[test]
fn exact_file_scope_does_not_match_similar_prefix_paths() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/foo.rs"), "needle target\n");
    write_text(&repo.path().join("src/foo.rs.bak"), "needle backup\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["needle", "src/foo.rs"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&output)), "needle target\n");
}

#[test]
fn binary_file_is_skipped_in_cli_search_results() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/text.rs"), "fn visible_text() {}\n");
    write_bytes(
        &repo.path().join("src/blob.bin"),
        b"prefix hidden\0needle suffix\n",
    );
    build_index(repo.path(), index.path());

    let text_hit = run_repo(repo.path(), index.path(), &["visible_text"]);
    assert_eq!(text_hit.status.code(), Some(0));
    assert!(fix_path(stdout_text(&text_hit)).contains("src/text.rs"));

    let binary_hit = run_repo(repo.path(), index.path(), &["needle"]);
    assert_eq!(binary_hit.status.code(), Some(1));
}

#[test]
fn non_utf8_file_content_matches_in_literal_and_regex_modes() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    let line = b"prefix\xFFneedle\x80suffix\n";
    write_bytes(&repo.path().join("src/non_utf8.txt"), line);
    build_index(repo.path(), index.path());

    let expected = b"src/non_utf8.txt:prefix\xFFneedle\x80suffix\n";

    let literal = run_repo(repo.path(), index.path(), &["-F", "needle"]);
    assert_eq!(literal.status.code(), Some(0));
    let mut actual_literal = literal.stdout;
    if cfg!(windows) {
        // Only fix the path part (before the first :)
        if let Some(pos) = actual_literal.iter().position(|&b| b == b':') {
            let mut fixed = actual_literal[..pos].to_vec();
            for b in &mut fixed {
                if *b == b'\\' {
                    *b = b'/';
                }
            }
            actual_literal.splice(..pos, fixed);
        }
    }
    assert_eq!(actual_literal, expected);

    let regex = run_repo(repo.path(), index.path(), &["(?-u)\\xFFneedle\\x80"]);
    assert_eq!(regex.status.code(), Some(0));
    let mut actual_regex = regex.stdout;
    if cfg!(windows) {
        if let Some(pos) = actual_regex.iter().position(|&b| b == b':') {
            let mut fixed = actual_regex[..pos].to_vec();
            for b in &mut fixed {
                if *b == b'\\' {
                    *b = b'/';
                }
            }
            actual_regex.splice(..pos, fixed);
        }
    }
    assert_eq!(actual_regex, expected);
}

#[test]
fn json_output_uses_bytes_fields_for_non_utf8_match_lines() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    let line = b"prefix\xFFneedle\x80suffix\n";
    write_bytes(&repo.path().join("src/non_utf8.txt"), line);
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--json", "(?-u)\\xFFneedle\\x80"],
    );
    assert_eq!(output.status.code(), Some(0));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|entry| serde_json::from_str(entry).expect("valid NDJSON line"))
        .collect();

    let matched = messages
        .iter()
        .find(|msg| msg["type"] == "match")
        .expect("match message");

    assert_eq!(
        fix_path(
            matched["data"]["path"]["text"]
                .as_str()
                .unwrap()
                .to_string()
        ),
        "src/non_utf8.txt"
    );
    assert!(matched["data"]["lines"]["text"].is_null());
    assert_eq!(
        matched["data"]["lines"]["bytes"],
        base64_encode(b"prefix\xFFneedle\x80suffix\n")
    );
    assert_eq!(matched["data"]["submatches"][0]["start"], 6);
    assert_eq!(matched["data"]["submatches"][0]["end"], 14);
    assert_eq!(
        matched["data"]["submatches"][0]["match"]["bytes"],
        base64_encode(b"\xFFneedle\x80")
    );
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn non_utf8_filename_is_reported_verbatim_in_flat_output() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    let file_name = OsString::from_vec(b"odd\xff.rs".to_vec());
    let file_path = repo.path().join("src").join(&file_name);
    write_text(&file_path, "needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["-F", "needle"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"src/odd\xff.rs:needle\n");
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn json_output_uses_bytes_fields_for_non_utf8_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    let file_name = OsString::from_vec(b"odd\xff.rs".to_vec());
    let file_path = repo.path().join("src").join(&file_name);
    write_text(&file_path, "needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "-F", "needle"]);
    assert_eq!(output.status.code(), Some(0));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|entry| serde_json::from_str(entry).expect("valid NDJSON line"))
        .collect();

    let begin = messages
        .iter()
        .find(|msg| msg["type"] == "begin")
        .expect("begin message");
    assert!(begin["data"]["path"]["text"].is_null());
    assert_eq!(
        begin["data"]["path"]["bytes"],
        base64_encode(b"src/odd\xff.rs")
    );

    let matched = messages
        .iter()
        .find(|msg| msg["type"] == "match")
        .expect("match message");
    assert!(matched["data"]["path"]["text"].is_null());
    assert_eq!(
        matched["data"]["path"]["bytes"],
        base64_encode(b"src/odd\xff.rs")
    );
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn non_utf8_filenames_work_with_glob_and_type_filters() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    let file_name = OsString::from_vec(b"odd\xff.rs".to_vec());
    let file_path = repo.path().join("src").join(&file_name);
    write_text(&file_path, "needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["-F", "-g", "src/**", "-t", "rs", "needle"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"src/odd\xff.rs:needle\n");
}

#[test]
fn utf16_le_file_is_searchable_via_cli_flat_output() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    // "fn utf16_cli_fn() {}\n" encoded as UTF-16 LE with BOM (FF FE)
    let text = "fn utf16_cli_fn() {}\n";
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE]; // BOM
    for ch in text.encode_utf16() {
        bytes.push((ch & 0xFF) as u8);
        bytes.push((ch >> 8) as u8);
    }
    write_bytes(&repo.path().join("src/utf16.rs"), &bytes);
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["-F", "utf16_cli_fn"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected match\nstdout: {}\nstderr: {}",
        stdout_text(&output),
        stderr_text(&output)
    );
    assert!(
        std::str::from_utf8(&output.stdout).is_ok(),
        "stdout is not valid UTF-8"
    );
    assert!(stdout_text(&output).contains("utf16_cli_fn"));
}

#[test]
fn utf8_bom_file_match_line_has_no_bom_bytes() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    // UTF-8 BOM (EF BB BF) + content
    let mut bytes = vec![0xEF_u8, 0xBB, 0xBF];
    bytes.extend_from_slice(b"fn bom_cli_fn() {}\n");
    write_bytes(&repo.path().join("src/bom.rs"), &bytes);
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["-F", "bom_cli_fn"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected match\nstdout: {}\nstderr: {}",
        stdout_text(&output),
        stderr_text(&output)
    );
    // BOM bytes must not appear in output
    assert!(
        !output.stdout.windows(3).any(|w| w == [0xEF, 0xBB, 0xBF]),
        "BOM bytes found in output: {:?}",
        &output.stdout[..output.stdout.len().min(32)]
    );
}

#[test]
fn utf16_le_file_context_output_is_utf8() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    // "// preamble\nfn ctx_utf16_fn() {}\n// postamble\n" encoded as UTF-16 LE with BOM
    let text = "// preamble\nfn ctx_utf16_fn() {}\n// postamble\n";
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE]; // BOM
    for ch in text.encode_utf16() {
        bytes.push((ch & 0xFF) as u8);
        bytes.push((ch >> 8) as u8);
    }
    write_bytes(&repo.path().join("src/ctx_utf16.rs"), &bytes);
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["-C", "1", "ctx_utf16_fn"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected match\nstdout: {}\nstderr: {}",
        stdout_text(&output),
        stderr_text(&output)
    );
    let stdout = std::str::from_utf8(&output.stdout)
        .expect("context output for UTF-16 file must be valid UTF-8");
    assert!(
        stdout.contains("ctx_utf16_fn"),
        "context output must contain matched symbol, got: {stdout:?}"
    );
}

#[test]
fn utf16_le_invert_match_output_is_utf8() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    // UTF-16 LE file with BOM containing multiple lines, some with "marker" and some without
    let utf16_text = "fn utf16_invert_fn() {}\n// marker\nfn other_fn() {}\n";
    let mut utf16_bytes: Vec<u8> = vec![0xFF, 0xFE]; // BOM
    for ch in utf16_text.encode_utf16() {
        utf16_bytes.push((ch & 0xFF) as u8);
        utf16_bytes.push((ch >> 8) as u8);
    }
    write_bytes(&repo.path().join("src/utf16_invert.rs"), &utf16_bytes);

    build_index(repo.path(), index.path());

    // Search with invert-match for "marker": should output lines from the UTF-16 file that do NOT contain "marker"
    let output = run_repo(repo.path(), index.path(), &["-v", "marker"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected match\nstdout: {}\nstderr: {}",
        stdout_text(&output),
        stderr_text(&output)
    );

    let stdout = std::str::from_utf8(&output.stdout)
        .expect("invert-match output for UTF-16 file must be valid UTF-8");
    assert!(
        stdout.contains("utf16_invert_fn"),
        "invert-match output must contain UTF-16 file content after transcoding, got: {stdout:?}"
    );
}

#[test]
fn invert_match_searches_full_scoped_corpus_without_positive_hits() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/invert.txt"), "alpha\nbeta\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["-v", "needle", "src/invert.txt"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&output)), "alpha\nbeta\n");
}

#[test]
fn invert_match_count_and_files_with_matches_follow_selected_lines() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/one.txt"),
        "needle\nkeep this\nneedle again\n",
    );
    write_text(&repo.path().join("src/two.txt"), "needle only\n");
    write_text(&repo.path().join("src/three.txt"), "keep me too\n");
    build_index(repo.path(), index.path());

    let count = run_repo(repo.path(), index.path(), &["-v", "-c", "needle", "src"]);
    assert_eq!(count.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&count)),
        "src/one.txt:1\nsrc/three.txt:1\n"
    );

    let files = run_repo(repo.path(), index.path(), &["-v", "-l", "needle", "src"]);
    assert_eq!(files.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&files)),
        "src/one.txt\nsrc/three.txt\n"
    );

    let without = run_repo(
        repo.path(),
        index.path(),
        &["-v", "--files-without-match", "needle", "src"],
    );
    assert_eq!(without.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&without)), "src/two.txt\n");
}

#[test]
fn files_without_match_lists_only_unmatched_files() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "needle\n");
    write_text(&repo.path().join("src/two.txt"), "alpha\n");
    write_text(&repo.path().join("src/three.txt"), "beta\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--files-without-match", "needle", "src"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&output)),
        "src/three.txt\nsrc/two.txt\n"
    );
}

#[test]
fn files_without_match_lists_all_files_on_no_matches() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "alpha\n");
    write_text(&repo.path().join("src/two.txt"), "beta\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--files-without-match", "nonexistent", "src"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&output)), "src/one.txt\nsrc/two.txt\n");
}

#[test]
fn files_without_match_quiet_is_silent_but_keeps_exit_code() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "alpha shared\n");
    write_text(&repo.path().join("src/two.txt"), "beta shared\n");
    build_index(repo.path(), index.path());

    // -q suppresses output; exit 0 because at least one file lacks "alpha".
    let out_some = run_repo(
        repo.path(),
        index.path(),
        &["-q", "--files-without-match", "alpha", "src"],
    );
    assert_eq!(out_some.status.code(), Some(0));
    assert_eq!(stdout_text(&out_some), "");

    // Every file matches "shared" -> no unmatched file -> exit 1, still silent.
    let out_none = run_repo(
        repo.path(),
        index.path(),
        &["-q", "--files-without-match", "shared", "src"],
    );
    assert_eq!(out_none.status.code(), Some(1));
    assert_eq!(stdout_text(&out_none), "");
}

#[test]
fn count_matches_counts_individual_matches_per_file() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "needle needle\nalpha\n");
    write_text(&repo.path().join("src/two.txt"), "needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--count-matches", "needle", "src"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&output)),
        "src/one.txt:2\nsrc/two.txt:1\n"
    );

    let no_filename = run_repo(
        repo.path(),
        index.path(),
        &["--count-matches", "-I", "needle", "src"],
    );
    assert_eq!(no_filename.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&no_filename)), "2\n1\n");
}

#[test]
fn only_matching_prints_each_non_empty_match_on_its_own_line() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/one.txt"),
        "needle needle\nalpha needle beta\n",
    );
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["-o", "needle", "src/one.txt"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fix_path(stdout_text(&output)), "needle\nneedle\nneedle\n");

    let numbered = run_repo(
        repo.path(),
        index.path(),
        &["-o", "-n", "-H", "needle", "src/one.txt"],
    );
    assert_eq!(numbered.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&numbered)),
        "src/one.txt:1:needle\nsrc/one.txt:1:needle\nsrc/one.txt:2:needle\n"
    );
}

#[test]
fn only_matching_with_context_keeps_context_lines() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/one.txt"),
        "before\nneedle needle\nafter\n",
    );
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["-o", "-n", "-C", "1", "needle", "src/one.txt"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_text(&output),
        "1-before\n2:needle\n2:needle\n3-after\n"
    );
}

#[test]
fn only_matching_with_heading_groups_results_by_file() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "needle needle\n");
    write_text(&repo.path().join("src/two.txt"), "needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--heading", "-n", "-o", "needle", "src"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&output)),
        "src/one.txt\n1:needle\n1:needle\n\nsrc/two.txt\n1:needle\n"
    );
}

#[test]
fn only_matching_with_heading_and_context_groups_results_by_file() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/one.txt"),
        "before\nneedle needle\nafter\n",
    );
    write_text(&repo.path().join("src/two.txt"), "x\nneedle\ny\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--heading", "-n", "-o", "-C", "1", "needle", "src"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fix_path(stdout_text(&output)),
        "src/one.txt\n1-before\n2:needle\n2:needle\n3-after\n\nsrc/two.txt\n1-x\n2:needle\n3-y\n"
    );
}

#[test]
fn count_with_only_matching_acts_like_count_matches() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/one.txt"), "needle needle\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["-c", "-o", "needle", "src/one.txt"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_text(&output), "2\n");
}

#[test]
fn invert_match_json_emits_match_messages_with_empty_submatches() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/invert.json"),
        "alpha\nneedle\nbeta\n",
    );
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--json", "-v", "needle", "src/invert.json"],
    );
    assert_eq!(output.status.code(), Some(0));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();

    let kinds: Vec<_> = messages
        .iter()
        .map(|msg| msg["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["begin", "match", "match", "end", "summary"]);

    let match_messages: Vec<_> = messages
        .iter()
        .filter(|msg| msg["type"] == "match")
        .collect();
    assert_eq!(match_messages.len(), 2);
    assert_eq!(match_messages[0]["data"]["lines"]["text"], "alpha\n");
    assert_eq!(
        match_messages[0]["data"]["submatches"],
        serde_json::json!([])
    );
    assert_eq!(match_messages[1]["data"]["lines"]["text"], "beta\n");
    assert_eq!(
        match_messages[1]["data"]["submatches"],
        serde_json::json!([])
    );

    let end = messages
        .iter()
        .find(|msg| msg["type"] == "end")
        .expect("end message");
    assert_eq!(end["data"]["stats"]["matched_lines"], 2);
    assert_eq!(end["data"]["stats"]["matches"], 0);

    let summary = messages.last().expect("summary");
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["data"]["stats"]["matched_lines"], 2);
    assert_eq!(summary["data"]["stats"]["matches"], 0);

    let raw_lines = stdout_lines_with_newlines(&output);
    let expected_bytes: usize = raw_lines
        .iter()
        .zip(messages.iter())
        .filter(|(_, msg)| matches!(msg["type"].as_str(), Some("begin") | Some("match")))
        .map(|(line, _)| line.len())
        .sum();
    assert_eq!(end["data"]["stats"]["bytes_printed"], expected_bytes);
    assert_eq!(summary["data"]["stats"]["bytes_printed"], expected_bytes);
}

#[test]
fn invert_match_json_searches_full_scoped_corpus_without_positive_hits() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/full.json"), "alpha\nbeta\n");
    build_index(repo.path(), index.path());

    let output = run_repo(
        repo.path(),
        index.path(),
        &["--json", "-v", "needle", "src/full.json"],
    );
    assert_eq!(output.status.code(), Some(0));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid NDJSON line"))
        .collect();

    let match_messages: Vec<_> = messages
        .iter()
        .filter(|msg| msg["type"] == "match")
        .collect();
    assert_eq!(match_messages.len(), 2);
    assert_eq!(match_messages[0]["data"]["lines"]["text"], "alpha\n");
    assert_eq!(match_messages[1]["data"]["lines"]["text"], "beta\n");
}

#[test]
fn broken_pipe_exits_cleanly_instead_of_panicking() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();

    let mut content = String::new();
    for i in 0..5000 {
        content.push_str(&format!("fn repeated_symbol_{i}() {{ /* needle */ }}\n"));
    }
    write_text(&repo.path().join("src/many.rs"), &content);
    build_index(repo.path(), index.path());

    let mut child = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index.path())
        .arg("needle")
        // Pin stdin so a piped test-harness stdin cannot engage the stdin
        // filter; this test targets broken-pipe handling, not stdin search.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rl");

    let mut first_line = String::new();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    stdout.read_line(&mut first_line).unwrap();
    assert!(first_line.contains("needle"));
    drop(stdout);

    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut stderr).unwrap();
    }
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "stderr:\n{stderr}");
    assert!(
        !stderr.contains("Broken pipe") && !stderr.contains("panicked"),
        "stderr:\n{stderr}"
    );
}

// --- New flag integration tests ---

#[test]
fn smart_case_lowercase_pattern_matches_mixed_case() {
    let repo = tempfile::TempDir::new().unwrap();
    let idx = tempfile::TempDir::new().unwrap();
    fs::write(repo.path().join("a.txt"), "Hello World\n").unwrap();
    build_index(repo.path(), idx.path());

    // -S with all-lowercase pattern: should match "Hello World" case-insensitively
    let out = run_repo(repo.path(), idx.path(), &["-S", "hello"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout_text(&out).contains("Hello World"));

    // Without -S, "hello" should NOT match "Hello World" (case-sensitive default)
    let out = run_repo(repo.path(), idx.path(), &["hello"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "case-sensitive should not match"
    );

    // -S with mixed-case pattern: should still match exact case
    let out = run_repo(repo.path(), idx.path(), &["-S", "Hello"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout_text(&out).contains("Hello World"));
}

#[test]
fn null_separator_in_files_with_matches() {
    let repo = tempfile::TempDir::new().unwrap();
    let idx = tempfile::TempDir::new().unwrap();
    fs::write(repo.path().join("a.rs"), "needle\n").unwrap();
    fs::write(repo.path().join("b.rs"), "needle\n").unwrap();
    build_index(repo.path(), idx.path());

    let out = run_repo(repo.path(), idx.path(), &["-l", "--null", "needle"]);
    assert_eq!(out.status.code(), Some(0));
    // NUL-terminated: output should contain NUL bytes, no newlines
    assert!(out.stdout.contains(&b'\0'), "expected NUL bytes in output");
    assert!(
        !out.stdout.contains(&b'\n'),
        "expected no newlines when --null is set"
    );
    // Two files → two NUL terminators
    assert_eq!(out.stdout.iter().filter(|&&b| b == b'\0').count(), 2);
}

#[test]
fn stats_flag_writes_to_stderr() {
    let repo = tempfile::TempDir::new().unwrap();
    let idx = tempfile::TempDir::new().unwrap();
    fs::write(repo.path().join("a.rs"), "needle\n").unwrap();
    build_index(repo.path(), idx.path());

    let out = run_repo(repo.path(), idx.path(), &["--stats", "needle"]);
    assert_eq!(out.status.code(), Some(0));
    let err = stderr_text(&out);
    assert!(
        err.contains("Elapsed:"),
        "expected Elapsed in stats: {err:?}"
    );
    assert!(
        err.contains("Matches: 1"),
        "expected match count in stats: {err:?}"
    );
    assert!(
        err.contains("Files with matches: 1"),
        "expected file count in stats: {err:?}"
    );
}

#[test]
fn files_flag_lists_indexed_paths() {
    let repo = tempfile::TempDir::new().unwrap();
    let idx = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/lib.rs"), "// lib\n").unwrap();
    fs::write(repo.path().join("src/main.rs"), "// main\n").unwrap();
    fs::write(repo.path().join("README.md"), "# readme\n").unwrap();
    build_index(repo.path(), idx.path());

    let out = run_repo(repo.path(), idx.path(), &["--files"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = fix_path(stdout_text(&out));
    assert!(
        stdout.contains("src/lib.rs"),
        "expected src/lib.rs in --files output"
    );
    assert!(
        stdout.contains("src/main.rs"),
        "expected src/main.rs in --files output"
    );
    assert!(
        stdout.contains("README.md"),
        "expected README.md in --files output"
    );
}

#[test]
fn files_flag_lists_freshly_created_untracked_file_without_manual_update() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);

    fs::write(repo.path().join("a.rs"), "fn hello() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    // Write a new untracked file after the index was built. No `st update`
    // is run before the `--files` call below: the bounded auto-update in
    // `cmd_files` (routed through `catchup::run_bounded_auto_update`, the
    // same helper `cmd_search` uses) must pick it up via git detection.
    fs::write(repo.path().join("b.rs"), "fn brand_new_file() {}\n").unwrap();

    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index_dir)
        .env("SYNTEXT_NO_ASYNC_UPDATE", "1")
        // Slow git spawns on Windows CI starve the default 150ms budget before
        // ls-files --others detects untracked b.rs; give detection room.
        .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000")
        .arg("--files")
        // Filter by extension via --glob; positionals to --files are path scope
        // (rg semantics), not globs, so `*.rs` as a bare arg would match nothing.
        .arg("--glob")
        .arg("*.rs")
        .output()
        .expect("run st --files");

    assert_eq!(out.status.code(), Some(0));
    let stdout = fix_path(stdout_text(&out));
    assert!(
        stdout.contains("b.rs"),
        "expected freshly created b.rs to be listed by --files without a manual `st update`, got:\n{}",
        stdout
    );
}

#[test]
fn invert_match_reflects_freshly_created_untracked_file_without_manual_update() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);

    fs::write(repo.path().join("a.rs"), "fn hello() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    // Write a new untracked file after the index was built. No `st update`
    // is run before the `-v` (invert-match) call below: the bounded
    // auto-update in `render_invert_match` (routed through
    // `catchup::run_bounded_auto_update`, the same helper `cmd_search` and
    // `cmd_files` use) must pick it up via git detection so the scoped-path
    // walk includes it.
    fs::write(repo.path().join("b.rs"), "fn brand_new_file() {}\n").unwrap();

    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index_dir)
        .env("SYNTEXT_NO_ASYNC_UPDATE", "1")
        // Slow git spawns on Windows CI starve the default 150ms budget before
        // ls-files --others detects untracked b.rs; give detection room.
        .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000")
        .arg("-v")
        .arg("-l")
        .arg("needle")
        .arg("b.rs")
        .output()
        .expect("run st -v -l");

    assert_eq!(out.status.code(), Some(0));
    let stdout = fix_path(stdout_text(&out));
    assert_eq!(
        stdout, "b.rs\n",
        "expected freshly created b.rs to be listed by invert-match without a manual `st update`, got:\n{}",
        stdout
    );
}

#[test]
fn type_list_prints_known_types() {
    let out = run(&["--type-list"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_text(&out);
    assert!(
        stdout.contains("rust:"),
        "expected 'rust:' in --type-list output"
    );
    assert!(
        stdout.contains("python:"),
        "expected 'python:' in --type-list output"
    );
}

#[test]
fn pcre2_warns_but_searches_normally() {
    let repo = tempfile::TempDir::new().unwrap();
    let idx = tempfile::TempDir::new().unwrap();
    fs::write(repo.path().join("a.rs"), "foo bar\n").unwrap();
    build_index(repo.path(), idx.path());

    let out = run_repo(repo.path(), idx.path(), &["-P", "foo"]);
    assert_eq!(out.status.code(), Some(0));
    let err = stderr_text(&out);
    assert!(
        err.contains("--pcre2 is not supported"),
        "expected pcre2 warning in stderr: {err:?}"
    );
    assert!(
        stdout_text(&out).contains("foo bar"),
        "expected match output despite pcre2 flag"
    );
}

fn tool_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn fallback_to_ripgrep_when_index_missing() {
    if !tool_available("rg") {
        eprintln!("skipping fallback_to_ripgrep_when_index_missing: rg not in PATH");
        return;
    }
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext"); // never created -> IndexNotFound
    write_text(&repo.path().join("a.rs"), "let FALLBACKNEEDLE = 1;\n");

    // Fallback is default-on: no env var, no --fallback flag.
    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .env_remove("SYNTEXT_FALLBACK_RG")
        .arg("FALLBACKNEEDLE")
        .arg(repo.path())
        .output()
        .expect("run st");

    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr_text(&out));
    assert!(
        stdout_text(&out).contains("FALLBACKNEEDLE"),
        "expected rg result on stdout:\n{}",
        stdout_text(&out)
    );
    assert!(
        stderr_text(&out).contains("ripgrep fallback"),
        "expected fallback notice on stderr:\n{}",
        stderr_text(&out)
    );
}

#[test]
fn missing_index_fallback_disabled_errors_with_guidance() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext"); // never created -> IndexNotFound

    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .env("SYNTEXT_FALLBACK_RG", "0")
        .arg("anything")
        .output()
        .expect("run st");

    assert_eq!(out.status.code(), Some(2));
    let err = stderr_text(&out);
    assert!(err.contains("no index found"), "stderr:\n{err}");
    assert!(err.contains("SYNTEXT_FALLBACK_RG"), "stderr:\n{err}");
}

#[test]
fn auto_update_over_max_files_emits_notice_and_searches_normally() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);

    let a_path = repo.path().join("a.rs");
    fs::write(&a_path, "fn hello() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    // Write 4 files to exceed the auto-update limit of 2.
    for i in 0..4 {
        fs::write(
            repo.path().join(format!("mod_{i}.rs")),
            format!("fn mod_{i}() {{ /* marker_{i} */ }}\n"),
        )
        .unwrap();
    }

    // Run st search with SYNTEXT_AUTO_UPDATE_MAX_FILES=2 and the async
    // catch-up disabled, so this test only observes the synchronous notice
    // (the spawn itself is covered by its own test below).
    // Stderr should contain the exact staleness notice, and nothing about it
    // should leak into stdout.
    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index_dir)
        .env("SYNTEXT_AUTO_UPDATE_MAX_FILES", "2")
        .env("SYNTEXT_NO_ASYNC_UPDATE", "1")
        .arg("hello")
        .output()
        .expect("run st");

    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_text(&out);
    let stderr = stderr_text(&out);
    assert!(
        stdout.contains("fn hello()"),
        "expected stdout to contain results, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("files behind") && !stdout.contains("searching stale"),
        "notice must not leak into stdout, got: {}",
        stdout
    );
    assert!(
        stderr.contains("st: index is ~4 files behind; searching stale (run 'st update')"),
        "expected stderr to contain warning notice, got: {}",
        stderr
    );

    // Now run with --quiet, warning should be suppressed and stderr should be empty.
    let out_quiet = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index_dir)
        .env("SYNTEXT_AUTO_UPDATE_MAX_FILES", "2")
        .env("SYNTEXT_NO_ASYNC_UPDATE", "1")
        .arg("--quiet")
        .arg("hello")
        .output()
        .expect("run st");

    assert_eq!(out_quiet.status.code(), Some(0));
    let stderr_quiet = stderr_text(&out_quiet);
    assert!(
        stderr_quiet.is_empty(),
        "expected stderr to be empty under --quiet, got: {}",
        stderr_quiet
    );
}

/// The `core.fsmonitor` tip fires whenever git detection eats more than half
/// the auto-update budget, which is load-dependent: on a loaded machine it
/// used to leak onto stderr under `--quiet`, whose contract is an empty
/// stderr. Forcing a 1ms budget makes the tip's precondition hold on every
/// run, so an ungated tip fails this deterministically instead of ~1 run in
/// 12 under load. The quiet run goes first on purpose: the tip is one-shot
/// per index (stamp file), so a control run ahead of it would mask the leak.
#[test]
fn quiet_suppresses_the_fsmonitor_tip() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);
    write_text(&repo.path().join("a.rs"), "fn tip_probe() {}\n");
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    let run = |quiet: bool| {
        let mut cmd = st();
        cmd.arg("--repo-root")
            .arg(repo.path())
            .arg("--index-dir")
            .arg(&index_dir)
            // 1ms budget: any real `git` invocation overshoots half of it, so
            // the tip's threshold is met on every run.
            .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "1")
            .env("SYNTEXT_NO_ASYNC_UPDATE", "1");
        if quiet {
            cmd.arg("--quiet");
        }
        cmd.arg("tip_probe").output().expect("run st")
    };

    let stamp = index_dir.join("fsmonitor-tip-shown");
    let quiet_out = run(true);
    assert_eq!(quiet_out.status.code(), Some(0));
    assert_eq!(
        stderr_text(&quiet_out),
        "",
        "--quiet must not print the fsmonitor tip"
    );
    assert!(
        !stamp.exists(),
        "a suppressed tip must not burn the one-shot stamp"
    );

    // Control: the same invocation without --quiet does emit the tip, which
    // is what proves the assertions above were not vacuous.
    let loud_out = run(false);
    assert_eq!(loud_out.status.code(), Some(0));
    assert!(
        stderr_text(&loud_out).contains("core.fsmonitor"),
        "expected the fsmonitor tip without --quiet, got: {}",
        stderr_text(&loud_out)
    );
}

/// A search that cannot get the index dir's shared lock retries on a bounded
/// schedule (`cli::open_retry`) before giving up, because `st` creates that
/// contention itself: a budget-overrunning search spawns a detached
/// `st update --quiet` whose exclusive window can swallow the *next* search's
/// open and turn it into exit 2.
///
/// This covers the user-visible contract only -- exit 2 and the same message,
/// with the lock held for the child's whole lifetime, so the outcome cannot
/// change. It deliberately does not wall-clock the child to prove the retry
/// ran: `st` exec latency alone reaches multiple seconds while the OS
/// assesses a freshly-linked binary, which swamps the 500ms schedule. That
/// half is asserted in-process by `cli::open_retry`'s tests.
#[test]
fn search_of_a_locked_index_fails_loudly() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    write_text(&repo.path().join("a.rs"), "fn lock_probe() {}\n");
    build_index(repo.path(), &index_dir);

    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(index_dir.join("lock"))
        .expect("open index dir lock");
    held.try_lock().expect("take exclusive index dir lock");

    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index_dir)
        .args(["--no-update", "lock_probe"])
        .output()
        .expect("run st");
    held.unlock().expect("release index dir lock");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a lock held for the whole run must still fail loudly, got stderr: {}",
        stderr_text(&out)
    );
    assert!(
        stderr_text(&out).contains("locked by another process"),
        "expected the lock-conflict message, got: {}",
        stderr_text(&out)
    );
}

/// Resolves the real `git` binary from `PATH` so the logging shim below can
/// exec through to it and preserve real detection behavior.
#[cfg(unix)]
mod git_shim_support {
    pub(super) fn resolve_real_git() -> Option<String> {
        let path_var = std::env::var("PATH").ok()?;
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("git");
            if candidate.is_file() {
                return candidate.to_str().map(|s| s.to_string());
            }
        }
        None
    }
}

/// Counts complete lines in `path`, treating a missing file as zero.
#[cfg(unix)]
fn count_lines(path: &Path) -> usize {
    match fs::read_to_string(path) {
        Ok(s) => s.lines().count(),
        Err(_) => 0,
    }
}

/// End-to-end proof that a stale search spawns a detached `st update --quiet`
/// catch-up: the spawned child runs its own three git detection commands
/// (`diff HEAD`, `diff --cached`, `ls-files --others`), which is observable
/// as extra lines appended to a logging `git` shim's log file after the
/// parent search has already returned. This sidesteps the separate,
/// documented limitation that overlay/pending updates from `commit_batch`
/// are process-local only (see `Manifest::overlay_gen`'s doc comment) --
/// this test only proves the detached process is spawned and does real git
/// work, not that its edits are visible to a later process.
#[cfg(unix)]
#[test]
fn stale_search_spawns_async_catchup_git_child() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);

    write_text(&repo.path().join("a.rs"), "fn hello() {}\n");
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    for i in 0..4 {
        write_text(
            &repo.path().join(format!("mod_{i}.rs")),
            &format!("fn mod_{i}() {{ /* marker_{i} */ }}\n"),
        );
    }

    let real_git = git_shim_support::resolve_real_git().unwrap_or_else(|| "git".to_string());
    let bin_dir = tempfile::TempDir::new().unwrap();
    let log_path = bin_dir.path().join("git_invocations.log");
    let shim = bin_dir.path().join("git");
    write_text(
        &shim,
        &format!(
            "#!/bin/sh\necho \"$@\" >> \"{log}\"\nexec \"{real}\" \"$@\"\n",
            log = log_path.display(),
            real = real_git
        ),
    );
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let real_path = std::env::var("PATH").unwrap_or_default();
    let shim_path = format!("{}:{}", bin_dir.path().display(), real_path);

    // Default config: auto_update_async_catchup is true, so this triggers
    // the detached `st update --quiet` spawn once results are printed. The
    // parent's own bounded detection contributes exactly 3 log lines.
    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index_dir)
        .env("PATH", &shim_path)
        .env("SYNTEXT_AUTO_UPDATE_MAX_FILES", "2")
        // The logging shim adds a `/bin/sh` fork per git call; give detection
        // a generous budget so that overhead never trips BudgetExceeded and
        // masks the TooManyFiles outcome this test depends on.
        .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000")
        .arg("hello")
        .output()
        .expect("run st");
    assert!(
        stderr_text(&out).contains("files behind"),
        "expected the initial search to report staleness, got: {}",
        stderr_text(&out)
    );

    let parent_git_calls = count_lines(&log_path);
    assert_eq!(
        parent_git_calls, 3,
        "expected exactly the parent's 3 detection calls before any catch-up runs"
    );

    // Poll the log file until the detached child's own (unlimited)
    // `update_from_git` has logged all 3 of its git detection calls. Waiting
    // for the full count (not just "more than before") means the child's
    // slowest work is done by the time this function returns, so it is much
    // less likely to still be forking git subprocesses -- and competing for
    // process-table slots -- while the next test in this binary starts.
    let mut saw_child_git_calls = false;
    for _ in 0..50 {
        if count_lines(&log_path) >= parent_git_calls + 3 {
            saw_child_git_calls = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        saw_child_git_calls,
        "expected the detached `st update` child to run its own git detection within 5s; log:\n{}",
        fs::read_to_string(&log_path).unwrap_or_default()
    );
    // Give the child a little more time to finish `commit_batch` (no further
    // git calls, just file I/O) and exit before this test's TempDir is
    // dropped out from under it.
    std::thread::sleep(std::time::Duration::from_millis(300));
}

/// `SYNTEXT_NO_ASYNC_UPDATE=1` must suppress the spawn entirely: no extra
/// git invocations ever show up in the shim log beyond the parent's own 3,
/// even after waiting past the window the spawn test uses to detect them.
#[cfg(unix)]
#[test]
fn no_async_update_env_suppresses_the_spawn() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);

    write_text(&repo.path().join("a.rs"), "fn hello() {}\n");
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    for i in 0..4 {
        write_text(
            &repo.path().join(format!("mod_{i}.rs")),
            &format!("fn mod_{i}() {{ /* marker_{i} */ }}\n"),
        );
    }

    let real_git = git_shim_support::resolve_real_git().unwrap_or_else(|| "git".to_string());
    let bin_dir = tempfile::TempDir::new().unwrap();
    let log_path = bin_dir.path().join("git_invocations.log");
    let shim = bin_dir.path().join("git");
    write_text(
        &shim,
        &format!(
            "#!/bin/sh\necho \"$@\" >> \"{log}\"\nexec \"{real}\" \"$@\"\n",
            log = log_path.display(),
            real = real_git
        ),
    );
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let real_path = std::env::var("PATH").unwrap_or_default();
    let shim_path = format!("{}:{}", bin_dir.path().display(), real_path);

    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index_dir)
        .env("PATH", &shim_path)
        .env("SYNTEXT_NO_ASYNC_UPDATE", "1")
        .env("SYNTEXT_AUTO_UPDATE_MAX_FILES", "2")
        .env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000")
        .arg("hello")
        .output()
        .expect("run st");
    assert!(
        stderr_text(&out).contains("files behind"),
        "expected the notice on the initial stale search"
    );

    let parent_git_calls = count_lines(&log_path);
    assert_eq!(
        parent_git_calls, 3,
        "expected only the parent's 3 detection calls"
    );

    // Wait past the window the spawn test uses, then confirm no extra calls
    // ever landed: the count must stay pinned at exactly 3.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(
        count_lines(&log_path),
        3,
        "no background `st update` should have run under SYNTEXT_NO_ASYNC_UPDATE=1"
    );
}

#[test]
fn hook_rewritten_command_auto_updates_and_searches() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);

    let a_path = repo.path().join("a.rs");
    fs::write(&a_path, "fn hello() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    // Now write a new unindexed/untracked file with a unique pattern.
    let b_path = repo.path().join("b.rs");
    fs::write(&b_path, "fn hook_unindexed_marker() {}\n").unwrap();

    // Prepare hook stdin JSON
    let hook_input = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {
            "command": "rg hook_unindexed_marker",
            "description": "search"
        },
        "cwd": repo.path()
    });

    // Run __hook claude
    let mut child = st()
        .arg("__hook")
        .arg("claude")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(hook_input.to_string().as_bytes())
        .unwrap();
    // Close stdin so the hook's read-to-EOF sees EOF. Claude Code closes the
    // hook's stdin after sending the JSON payload; without this the hook
    // blocks forever on read_to_string and the test deadlocks on read_to_end.
    drop(child.stdin.take());
    let mut output_bytes = Vec::new();
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_to_end(&mut output_bytes)
        .unwrap();
    let exit_code = child.wait().unwrap().code();
    assert_eq!(exit_code, Some(0));

    let hook_output: serde_json::Value = serde_json::from_slice(&output_bytes).unwrap();
    let rewritten_cmd = hook_output["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();

    // The rewritten command has the form: "/path/to/st hook_unindexed_marker"
    let parts: Vec<&str> = rewritten_cmd.split_whitespace().collect();
    assert!(!parts.is_empty());

    let clean_exe = parts[0].trim_matches('\'').trim_matches('"');
    let mut run_cmd = Command::new(clean_exe);
    run_cmd.current_dir(repo.path());
    // Slow git spawns on Windows CI starve the default 150ms budget before
    // ls-files --others detects untracked b.rs; give detection room.
    run_cmd.env("SYNTEXT_AUTO_UPDATE_BUDGET_MS", "10000");
    for arg in &parts[1..] {
        let clean_arg = arg.trim_matches('\'').trim_matches('"');
        run_cmd.arg(clean_arg);
    }

    let search_output = run_cmd.output().unwrap();
    assert_eq!(search_output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&search_output.stdout);
    assert!(
        stdout.contains("b.rs"),
        "expected b.rs in results, got: {}",
        stdout
    );
    assert!(stdout.contains("hook_unindexed_marker"));
}

/// Auto-update failures must be invisible to search output: `cmd_search`
/// treats a broken git binary the same as "no changes detected" and falls
/// back to the stale (but still correct) index. This test forces git
/// resolution to find a bogus `git` (via a `PATH` override pointing at a
/// script that always exits non-zero) and asserts the exit code and stdout
/// are byte-identical to a run against a healthy git, for both a match and
/// a no-match query. Stderr is intentionally not compared: the two runs are
/// allowed to diverge there (see DIVERGENCES.md-style reasoning in
/// `cmd_search`'s auto-update match arms).
#[cfg(unix)]
#[test]
fn broken_git_binary_yields_identical_exit_code_and_stdout() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir(&index_dir).unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    fs::write(repo.path().join(".gitignore"), ".syntext/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-m", "ignore index", "--no-gpg-sign"]);

    write_text(&repo.path().join("a.rs"), "fn broken_git_needle() {}\n");
    git(&["add", "a.rs"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);

    build_index(repo.path(), &index_dir);

    // Bogus `git` that always fails to exec successfully: exercises the
    // git-detection failure path inside `update_from_git` while the search
    // itself must proceed unaffected.
    let fake_bin = tempfile::TempDir::new().unwrap();
    let fake_git = fake_bin.path().join("git");
    write_text(&fake_git, "#!/bin/sh\nexit 1\n");
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755)).unwrap();

    let real_path = std::env::var("PATH").unwrap_or_default();
    let broken_path = format!("{}:{}", fake_bin.path().display(), real_path);

    // Each run below auto-updates. Under load the 150ms detection budget is
    // exceeded, which spawns the detached `st update --quiet` catch-up; that
    // child holds the index dir's exclusive lock while a later run's
    // `Index::open` tries for a shared one, and search deliberately fails
    // loudly on LockConflict (exit 2). That is a real concurrency behavior,
    // but it is not this test's subject: pinning the catch-up off removes the
    // only concurrent writer so the git-health invariant is measured alone.
    let run_with_path = |query: &str, path: &str| {
        st().arg("--repo-root")
            .arg(repo.path())
            .arg("--index-dir")
            .arg(&index_dir)
            .env("PATH", path)
            .env("SYNTEXT_NO_ASYNC_UPDATE", "1")
            .arg(query)
            .output()
            .expect("run st")
    };

    // Match case: healthy git vs. broken git must agree on exit code and stdout.
    let healthy_match = run_with_path("broken_git_needle", &real_path);
    let broken_match = run_with_path("broken_git_needle", &broken_path);
    assert_eq!(healthy_match.status.code(), Some(0));
    assert_eq!(broken_match.status.code(), Some(0));
    assert_eq!(
        healthy_match.stdout, broken_match.stdout,
        "stdout must be byte-identical regardless of git health"
    );

    // No-match case: same invariant at exit code 1 / empty stdout.
    let healthy_nomatch = run_with_path("no_such_needle_xyz", &real_path);
    let broken_nomatch = run_with_path("no_such_needle_xyz", &broken_path);
    assert_eq!(healthy_nomatch.status.code(), Some(1));
    assert_eq!(broken_nomatch.status.code(), Some(1));
    assert_eq!(healthy_nomatch.stdout, broken_nomatch.stdout);
    assert!(broken_nomatch.stdout.is_empty());
}

/// `st update --quiet` is the command a git hook fires. A hook may run
/// before the repo has ever been indexed (e.g. `post-checkout` right after
/// clone), so a missing index must not make the hook noisy or fail loudly:
/// documented hook-safe behavior is exit 0 with empty stderr.
#[test]
fn update_quiet_with_no_index_exits_zero_with_empty_stderr() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext-missing");

    let output = run_repo(repo.path(), &index_dir, &["update", "--quiet"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "st update --quiet with no index must exit 0\nstderr:\n{}",
        stderr_text(&output)
    );
    assert!(
        output.stderr.is_empty(),
        "st update --quiet with no index must produce no stderr, got: {}",
        stderr_text(&output)
    );
}

/// `st agent install githooks --project` / `show` / `uninstall` round-trip
/// through the real git hooks directory of a temp git repo: install must
/// write the marker-delimited block into all four hook files, show must
/// report installed, and uninstall must strip the block back out.
#[test]
fn agent_githooks_install_show_uninstall_round_trip_in_temp_git_repo() {
    let repo = tempfile::TempDir::new().unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap()
    };
    git(&["init"]);

    let run_in_repo = |args: &[&str]| {
        let mut cmd = st();
        cmd.current_dir(repo.path()).args(args);
        cmd.output().expect("run st agent")
    };

    let hooks_dir = repo.path().join(".git/hooks");
    let hook_names = ["post-commit", "post-checkout", "post-merge", "post-rewrite"];

    // Show before install: not installed.
    let show_before = run_in_repo(&["agent", "show", "githooks", "--project"]);
    assert_eq!(show_before.status.code(), Some(0));
    assert!(
        stdout_text(&show_before).contains("missing"),
        "expected 'missing' before install, got: {}",
        stdout_text(&show_before)
    );

    // Install.
    let install = run_in_repo(&["agent", "install", "githooks", "--project"]);
    assert_eq!(
        install.status.code(),
        Some(0),
        "install failed\nstdout:\n{}\nstderr:\n{}",
        stdout_text(&install),
        stderr_text(&install)
    );
    for name in hook_names {
        let content = fs::read_to_string(hooks_dir.join(name)).unwrap_or_default();
        assert!(
            content.contains("syntext-agent:githooks:start"),
            "expected marker block in {name}, got: {content}"
        );
    }

    // Show after install: installed.
    let show_after = run_in_repo(&["agent", "show", "githooks", "--project"]);
    assert_eq!(show_after.status.code(), Some(0));
    assert!(
        stdout_text(&show_after).contains("installed"),
        "expected 'installed' after install, got: {}",
        stdout_text(&show_after)
    );

    // Uninstall.
    let uninstall = run_in_repo(&["agent", "uninstall", "githooks", "--project"]);
    assert_eq!(
        uninstall.status.code(),
        Some(0),
        "uninstall failed\nstdout:\n{}\nstderr:\n{}",
        stdout_text(&uninstall),
        stderr_text(&uninstall)
    );
    for name in hook_names {
        let content = fs::read_to_string(hooks_dir.join(name)).unwrap_or_default();
        assert!(
            !content.contains("syntext-agent:githooks:start"),
            "expected marker block removed from {name}, got: {content}"
        );
    }

    // Show after uninstall: back to missing.
    let show_final = run_in_repo(&["agent", "show", "githooks", "--project"]);
    assert_eq!(show_final.status.code(), Some(0));
    assert!(
        stdout_text(&show_final).contains("missing"),
        "expected 'missing' after uninstall, got: {}",
        stdout_text(&show_final)
    );
}

/// `st init --fsmonitor` must set `core.fsmonitor=true` in the enclosing git
/// repository, asserted directly via a `git config --get` subprocess (not
/// just the tool's own detection helper), and must never touch the setting
/// when the flag is absent.
///
/// Manual residual: the interactive prompt path (offering to enable
/// fsmonitor without the flag) is not exercised here; only the flag path is,
/// since a real prompt requires a TTY this subprocess harness does not have.
#[test]
fn init_fsmonitor_flag_sets_core_fsmonitor_in_temp_repo() {
    let repo = tempfile::TempDir::new().unwrap();
    Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init"])
        .output()
        .unwrap();

    let git_config_fsmonitor = || {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["config", "--get", "core.fsmonitor"])
            .output()
            .unwrap()
    };

    // Baseline: unset before `st init` runs at all.
    let before = git_config_fsmonitor();
    assert!(
        !before.status.success(),
        "core.fsmonitor must be unset before st init runs"
    );

    // `st init` with no --fsmonitor must never set it (opt-in only).
    let no_flag = Command::new(env!("CARGO_BIN_EXE_st"))
        .current_dir(repo.path())
        .args(["init"])
        .output()
        .expect("run st init");
    assert_eq!(
        no_flag.status.code(),
        Some(0),
        "st init failed\nstdout:\n{}\nstderr:\n{}",
        stdout_text(&no_flag),
        stderr_text(&no_flag)
    );
    let still_unset = git_config_fsmonitor();
    assert!(
        !still_unset.status.success(),
        "core.fsmonitor must stay unset when --fsmonitor is not passed"
    );

    // `st init --fsmonitor` must set it.
    let with_flag = Command::new(env!("CARGO_BIN_EXE_st"))
        .current_dir(repo.path())
        .args(["init", "--fsmonitor"])
        .output()
        .expect("run st init --fsmonitor");
    assert_eq!(
        with_flag.status.code(),
        Some(0),
        "st init --fsmonitor failed\nstdout:\n{}\nstderr:\n{}",
        stdout_text(&with_flag),
        stderr_text(&with_flag)
    );
    assert!(
        stdout_text(&with_flag).contains("enabled core.fsmonitor"),
        "expected confirmation message, got: {}",
        stdout_text(&with_flag)
    );
    let after = git_config_fsmonitor();
    assert!(
        after.status.success(),
        "core.fsmonitor must be set after st init --fsmonitor"
    );
    assert_eq!(
        String::from_utf8_lossy(&after.stdout).trim(),
        "true",
        "core.fsmonitor must be set to true"
    );
}

/// Installing the git hooks integration and then running a *real* `git
/// commit` must actually trigger `st update` in the background via the
/// installed post-commit hook, landing new content in the index with no
/// explicit `st update` call from the test. The index is built with no
/// `--index-dir` override so it resolves to the default `<repo>/.syntext`
/// location, matching what the hook-triggered `st update` (invoked with no
/// flags, cwd = repo root) will also resolve to. Polling searches disable
/// their own in-band auto-update (`SYNTEXT_NO_AUTO_UPDATE=1`) so a positive
/// result can only come from the hook's background update, not from the
/// search's own bounded catch-up.
#[test]
fn githooks_post_commit_hook_triggers_background_update() {
    let repo = tempfile::TempDir::new().unwrap();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap()
    };
    git(&["init"]);
    fs::write(repo.path().join("a.rs"), "fn original_marker() {}\n").unwrap();
    git(&["add", "-A"]);
    let initial_commit = git(&["commit", "-m", "initial", "--no-gpg-sign"]);
    assert!(
        initial_commit.status.success(),
        "initial commit failed: {}",
        String::from_utf8_lossy(&initial_commit.stderr)
    );

    let run_in_repo = |args: &[&str]| {
        let mut cmd = st();
        cmd.current_dir(repo.path()).args(args);
        cmd.output().expect("run st")
    };

    let index_build = run_in_repo(&["index", "--quiet"]);
    assert_eq!(
        index_build.status.code(),
        Some(0),
        "st index failed: {}",
        stderr_text(&index_build)
    );

    let install = run_in_repo(&["agent", "install", "githooks", "--project"]);
    assert_eq!(
        install.status.code(),
        Some(0),
        "hook install failed: {}",
        stderr_text(&install)
    );
    let post_commit_hook = repo.path().join(".git/hooks/post-commit");
    assert!(
        post_commit_hook.exists(),
        "post-commit hook should exist after install"
    );

    // New file, staged and committed for real: the installed post-commit hook
    // fires and spawns a detached `st update --quiet &`.
    fs::write(
        repo.path().join("hooked.rs"),
        "fn githook_triggered_marker() {}\n",
    )
    .unwrap();
    git(&["add", "-A"]);
    let commit = git(&["commit", "-m", "add hooked file", "--no-gpg-sign"]);
    assert!(
        commit.status.success(),
        "git commit (with post-commit hook installed) failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    let poll_search = || {
        let mut cmd = st();
        cmd.current_dir(repo.path())
            .env("SYNTEXT_NO_AUTO_UPDATE", "1")
            .args(["-q", "githook_triggered_marker"]);
        cmd.output().expect("run st search")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut found = false;
    while std::time::Instant::now() < deadline {
        if poll_search().status.code() == Some(0) {
            found = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        found,
        "expected the post-commit hook's background `st update` to make \
         githook_triggered_marker searchable within the timeout"
    );

    // Uninstall so the marker-delimited block doesn't linger past the test.
    let uninstall = run_in_repo(&["agent", "uninstall", "githooks", "--project"]);
    assert_eq!(
        uninstall.status.code(),
        Some(0),
        "hook uninstall failed: {}",
        stderr_text(&uninstall)
    );
}

#[test]
fn glob_exclude_then_include_is_positional_last_match_wins() {
    // Regression for review issue #3: `combined_globs` previously appended all
    // --exclude values after every --glob/--include regardless of CLI position,
    // so `--exclude='*.rs' -g 'src/main.rs'` could never re-include — the
    // exclude always won. ripgrep's semantics are positional last-match-wins,
    // so the later `-g 'src/main.rs'` must re-include it.
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(repo.path().join("src/other.rs"), "fn other() {}\n").unwrap();
    build_index(repo.path(), &index_dir);

    let out = run_repo(
        repo.path(),
        &index_dir,
        &["--files", "--exclude=*.rs", "--glob=src/main.rs"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let stdout = fix_path(stdout_text(&out));
    assert!(
        stdout.contains("src/main.rs"),
        "the later --glob must re-include src/main.rs over the earlier --exclude; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("src/other.rs"),
        "the earlier --exclude '*.rs' must still drop src/other.rs; got:\n{stdout}"
    );
}

#[test]
fn glob_short_eq_form_matches_like_long_eq() {
    // Regression for review issue #6: clap accepts `-g=val` for a short
    // value-option and strips the leading `=` (storing `val`), but the
    // occurrence-order reconstructor re-parses argv and used to keep the `=`,
    // emitting `=src/main.rs`. The count-match gate still passed (1==1), so the
    // wrong pattern was trusted and matched nothing. Both `-g=val` and `-gval`
    // must match the same files as `--glob=val`.
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(repo.path().join("src/other.py"), "x = 1\n").unwrap();
    build_index(repo.path(), &index_dir);

    for form in ["--glob=src/main.rs", "-g=src/main.rs", "-gsrc/main.rs"] {
        let out = run_repo(repo.path(), &index_dir, &["--files", form]);
        assert_eq!(out.status.code(), Some(0), "{form}: {}", stderr_text(&out));
        let stdout = fix_path(stdout_text(&out));
        assert!(
            stdout.contains("src/main.rs"),
            "{form}: must match src/main.rs; got:\n{stdout}"
        );
        assert!(
            !stdout.contains("src/other.py"),
            "{form}: must not match src/other.py; got:\n{stdout}"
        );
    }
}

#[test]
fn double_dash_escape_searches_for_subcommand_name_word() {
    // Regression for review issue #18: `st index` rebuilds (subcommand), so a
    // user wanting to search for the word "index" must escape. `st -e index`
    // has always worked; the `--` separator is the more conventional escape
    // hatch (clap routes post-`--` positionals to `pattern`, skipping
    // subcommand matching). Verify `st -- index` finds the literal word
    // "index" rather than triggering a rebuild.
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(
        repo.path().join("src/main.rs"),
        "let index = 42;\nfn other() {}\n",
    )
    .unwrap();
    build_index(repo.path(), &index_dir);

    let out = run_repo(repo.path(), &index_dir, &["--no-update", "--", "index"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let stdout = fix_path(stdout_text(&out));
    assert!(
        stdout.contains("let index = 42;"),
        "`st -- index` must search for the word 'index'; got:\n{stdout}"
    );
}

#[test]
fn unsupported_search_flags_warn_but_still_search() {
    let repo = tempfile::TempDir::new().unwrap();
    let index_dir = repo.path().join(".syntext");
    fs::write(repo.path().join("a.rs"), "needle here\n").unwrap();
    build_index(repo.path(), &index_dir);

    // --pre/--engine/--encoding/--type-add are parsed but do nothing; each must
    // warn on stderr so a caller is not misled, while the search still runs.
    let out = run_repo(
        repo.path(),
        &index_dir,
        &[
            "--pre",
            "cat",
            "--engine",
            "pcre2",
            "--encoding",
            "utf16",
            "--type-add",
            "foo:*.x",
            "needle",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let stderr = stderr_text(&out);
    assert!(
        stderr.contains("st: --pre/--pre-glob is not implemented"),
        "expected --pre warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains("st: --engine 'pcre2' is not implemented"),
        "expected --engine warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains("st: --encoding 'utf16' is not implemented"),
        "expected --encoding warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains("st: --type-add/--type-clear is not implemented"),
        "expected --type-add warning, got:\n{stderr}"
    );
    assert!(
        stdout_text(&out).contains("needle here"),
        "search must still run despite the warnings"
    );
}

// ---------------------------------------------------------------------------
// -f/--file pattern files and --rust/--rs type selector
// ---------------------------------------------------------------------------

#[test]
fn pattern_file_ors_patterns_like_multi_e() {
    let dir = tempfile::TempDir::new().unwrap();
    let pats = dir.path().join("pats.txt");
    fs::write(&pats, "alpha\ngamma\n").unwrap();
    let out = run_with_stdin(
        &["-n", "-f", pats.to_str().unwrap(), "-"],
        b"alpha
beta
gamma
",
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "1:alpha\n3:gamma\n");
}

#[test]
fn pattern_file_empty_line_matches_everything() {
    // rg semantics: an interior empty line is an empty pattern, which
    // matches every line.
    let dir = tempfile::TempDir::new().unwrap();
    let pats = dir.path().join("pats.txt");
    fs::write(&pats, "alpha\n\ngamma\n").unwrap();
    let out = run_with_stdin(
        &["-n", "-f", pats.to_str().unwrap(), "-"],
        b"alpha
beta
gamma
",
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "1:alpha\n2:beta\n3:gamma\n");
}

#[test]
fn pattern_file_fixed_strings_escape_each_alternative() {
    let dir = tempfile::TempDir::new().unwrap();
    let pats = dir.path().join("pats.txt");
    fs::write(&pats, "al.ha\nga\n").unwrap();
    let out = run_with_stdin(
        &["-n", "-F", "-f", pats.to_str().unwrap(), "-"],
        b"alpha
al.ha
",
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    // Only the literal `al.ha` matches; under -F the dot is not a wildcard.
    assert_eq!(stdout_text(&out), "2:al.ha\n");
}

#[test]
fn pattern_file_missing_exits_2() {
    let out = run_with_stdin(&["-f", "/nonexistent-definitely-st"], b"x\n");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_text(&out).contains("-f/--file"),
        "stderr:\n{}",
        stderr_text(&out)
    );
}

#[test]
fn pattern_file_empty_exits_1_like_rg() {
    let dir = tempfile::TempDir::new().unwrap();
    let pats = dir.path().join("empty.pats");
    fs::write(&pats, "").unwrap();
    let out = run_with_stdin(&["-f", pats.to_str().unwrap()], b"x\n");
    assert_eq!(out.status.code(), Some(1), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "");
}

#[test]
fn rust_flag_selects_rs_files() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext");
    write_text(&repo.path().join("a.rs"), "RUSTNEEDLE\n");
    write_text(&repo.path().join("b.txt"), "RUSTNEEDLE\n");
    build_index(repo.path(), &index);

    for flag in ["--rust", "--rs"] {
        let out = run_repo(repo.path(), &index, &[flag, "-l", "RUSTNEEDLE"]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "flag {flag}: {}",
            stderr_text(&out)
        );
        assert_eq!(stdout_text(&out), "a.rs\n", "flag {flag}");
    }
}

// ---------------------------------------------------------------------------
// stdin filter mode (rg-style `cat ... | st pat`)
// ---------------------------------------------------------------------------

fn run_with_stdin(args: &[&str], input: &[u8]) -> Output {
    let mut child = st()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn st");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(input)
        .expect("write stdin");
    child.wait_with_output().expect("wait st")
}

#[test]
fn stdin_pipe_matches_without_filename_prefix() {
    let out = run_with_stdin(&["-n", "b"], b"a\nb\nb\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "2:b\n3:b\n");
}

#[test]
fn stdin_pipe_no_match_exits_1() {
    let out = run_with_stdin(&["zzq"], b"a\nb\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_text(&out), "");
}

#[test]
fn stdin_pipe_h_count_uses_stdin_label() {
    let out = run_with_stdin(&["-H", "-c", "b"], b"a\nb\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_text(&out), "<stdin>:1\n");
}

#[test]
fn stdin_pipe_count_is_bare_by_default() {
    let out = run_with_stdin(&["-c", "b"], b"a\nb\nb\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_text(&out), "2\n");
}

#[test]
fn stdin_pipe_column_without_n_prints_line_and_column() {
    // rg --column forces line:col:text even when piped stdout would
    // otherwise disable line numbers (former DIVERGENCES.md #17).
    let out = run_with_stdin(&["--column", "b"], b"a\nbb\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "2:1:bb\n");
}

#[test]
fn stdin_pipe_explicit_no_line_number_beats_column_line_numbers() {
    // rg -N --column prints col:text; an explicit -N wins over --column.
    let out = run_with_stdin(&["-N", "--column", "b"], b"a\nbb\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "1:bb\n");
}

#[test]
fn indexed_column_without_n_prints_line_and_column() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext");
    write_text(&repo.path().join("a.txt"), "alpha\nCOLUMNNEEDLE here\n");
    build_index(repo.path(), &index);

    let out = run_repo(repo.path(), &index, &["-H", "--column", "COLUMNNEEDLE"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "a.txt:2:1:COLUMNNEEDLE here\n");
}

#[test]
fn stdin_pipe_files_with_matches_uses_stdin_label() {
    let out = run_with_stdin(&["-l", "b"], b"a\nb\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_text(&out), "<stdin>\n");
}

#[test]
fn stdin_pipe_json_uses_stdin_label() {
    let out = run_with_stdin(&["--json", "foo"], b"a\nfoo\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let stdout = stdout_text(&out);
    assert!(
        stdout.contains("\"path\":{\"text\":\"<stdin>\"}"),
        "expected <stdin> path in json, got:\n{stdout}"
    );
}

#[test]
fn stdin_pipe_inverts_per_line() {
    let out = run_with_stdin(&["-v", "b"], b"a\nb\nc\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_text(&out), "a\nc\n");
}

#[test]
fn stdin_pipe_binary_content_prints_notice_like_rg() {
    // rg semantics: a NUL in the stream replaces all line output with the
    // binary notice, exit 0 (match position relative to the NUL is
    // irrelevant); no match at all stays silent with exit 1.
    let out = run_with_stdin(&["a"], b"a\0b\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(
        stdout_text(&out),
        "binary file matches (found \"\\0\" byte around offset 1)\n"
    );

    let out = run_with_stdin(&["zzq"], b"a\0b\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_text(&out), "");
}

#[test]
fn stdin_pipe_glob_filters_warn_and_are_ignored() {
    let out = run_with_stdin(&["-g", "*.rs", "-t", "rust", "a"], b"a\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "a\n");
    assert!(
        stderr_text(&out).contains("are ignored when reading stdin"),
        "expected stdin glob warning"
    );
}

#[test]
fn stdin_pipe_max_count_truncates() {
    let out = run_with_stdin(&["-m", "1", "-n", "b"], b"b\nb\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_text(&out), "1:b\n");
}

#[test]
fn stdin_pipe_after_context_prints_context_lines() {
    let out = run_with_stdin(&["-n", "-A", "1", "b"], b"b\nafter\nnope\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "1:b\n2-after\n");
}

#[test]
fn stdin_pipe_works_without_index_or_git_repo() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut child = st()
        .args(["hi"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn st");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"hi\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait st");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "hi\n");
}

/// An explicit `-` always means stdin, even when stdin is /dev/null (rg rule).
#[test]
fn stdin_dash_with_null_stdin_reads_empty_and_exits_1() {
    let out = st()
        .args(["zzq", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run st");
    assert_eq!(out.status.code(), Some(1), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "");
}

#[test]
fn stdin_dash_reads_piped_input() {
    let out = run_with_stdin(&["foo", "-"], b"x\nfoo\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "foo\n");
}

#[test]
fn stdin_dash_mixed_with_invert_exits_2() {
    // stdin -v is per-line; indexed -v is corpus-wide. They cannot merge.
    let out = run_with_stdin(&["-v", "x", "-", "somefile"], b"x\n");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_text(&out).contains("cannot be combined with other paths under -v"),
        "expected mixed-dash -v error"
    );
}

/// rg semantics for `-` mixed with real paths: search both, stdin results
/// ordered by the argv position of `-`.
#[test]
fn stdin_dash_mixed_with_paths_searches_both() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext");
    write_text(&repo.path().join("a.txt"), "INDEXNEEDLE\n");
    build_index(repo.path(), &index);

    let run = |args: &[&str], input: &[u8]| {
        let mut child = st()
            .arg("--repo-root")
            .arg(repo.path())
            .arg("--index-dir")
            .arg(&index)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn st");
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(input)
            .expect("write stdin");
        child.wait_with_output().expect("wait st")
    };

    // `-` first: stdin results precede index results, like rg.
    let out = run(&["-n", "-H", "NEEDLE", "-", "."], b"STDINNEEDLE\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(
        stdout_text(&out),
        "<stdin>:1:STDINNEEDLE\na.txt:1:INDEXNEEDLE\n"
    );

    // `-` last: index results first.
    let out = run(&["-n", "-H", "NEEDLE", ".", "-"], b"STDINNEEDLE\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(
        stdout_text(&out),
        "a.txt:1:INDEXNEEDLE\n<stdin>:1:STDINNEEDLE\n"
    );

    // Match on either side is enough for exit 0.
    let out = run(&["-n", "-H", "STDINNEEDLE", "-", "."], b"STDINNEEDLE\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "<stdin>:1:STDINNEEDLE\n");

    // No match on either side: exit 1.
    let out = run(&["-n", "-H", "zzq", "-", "."], b"plain\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_text(&out), "");
}

#[test]
fn stdin_dash_mixed_with_binary_stream_prints_notice_in_position() {
    // rg semantics for a binary stdin half in a mixed search: the notice
    // replaces the stdin lines but not the file results, in argv position.
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext");
    write_text(&repo.path().join("a.txt"), "INDEXNEEDLE\n");
    build_index(repo.path(), &index);

    let mut child = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .args(["-n", "-H", "NEEDLE", "-", "."])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn st");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"STDIN\0NEEDLE\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait st");

    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(
        stdout_text(&out),
        "<stdin>: binary file matches (found \"\\0\" byte around offset 5)\na.txt:1:INDEXNEEDLE\n"
    );
}

#[test]
fn stdin_dash_mixed_without_index_cannot_fallback() {
    // stdin was already consumed by the time the missing index is detected,
    // so the rg fallback (which would re-read stdin) must not run.
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext"); // never created
    write_text(&repo.path().join("a.txt"), "INDEXNEEDLE\n");

    let mut child = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .args(["-n", "NEEDLE", "-", "."])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn st");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"STDINNEEDLE\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait st");

    assert_eq!(out.status.code(), Some(2));
    let err = stderr_text(&out);
    assert!(err.contains("no index found"), "stderr:\n{err}");
    assert!(err.contains("cannot re-read stdin"), "stderr:\n{err}");
}

/// `st pat - -`: every path argument is a dash, so the index half has no
/// scope. rg reads the stream once (the second `-` sees EOF) and searches
/// nothing else; st must not fall back to a whole-repo index search.
#[test]
fn stdin_double_dash_searches_only_the_stream() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext");
    write_text(&repo.path().join("a.txt"), "INDEXNEEDLE\n");
    build_index(repo.path(), &index);

    let mut child = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .args(["--no-update", "-n", "-H", "NEEDLE", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn st");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"STDINNEEDLE\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait st");

    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    // Only the stream; a.txt must NOT leak in via an empty-scope index half.
    assert_eq!(stdout_text(&out), "<stdin>:1:STDINNEEDLE\n");
}

/// A missing explicit path is reported like rg (stderr, exit 2) WITHOUT
/// dropping the surviving inputs' output, including an already-consumed
/// stdin half.
#[test]
fn missing_explicit_path_still_prints_other_inputs() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext");
    write_text(&repo.path().join("a.txt"), "INDEXNEEDLE\n");
    build_index(repo.path(), &index);

    let out = run_repo(
        repo.path(),
        &index,
        &["--no-update", "-n", "-H", "NEEDLE", "a.txt", "missing.txt"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_text(&out).contains("missing.txt: No such file"),
        "stderr:\n{}",
        stderr_text(&out)
    );
    assert_eq!(stdout_text(&out), "a.txt:1:INDEXNEEDLE\n");

    // Mixed `-`: the stdin half must survive the missing-path error too.
    let mut child = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .args([
            "--no-update",
            "-n",
            "-H",
            "NEEDLE",
            "-",
            "a.txt",
            "missing.txt",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn st");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"STDINNEEDLE\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait st");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stdout_text(&out),
        "<stdin>:1:STDINNEEDLE\na.txt:1:INDEXNEEDLE\n"
    );
}

/// A zero-width regex hit (submatch 0..0) is NOT an inverted match: -o must
/// not misread it as the -v sentinel and print the whole line.
#[test]
fn only_matching_zero_width_hit_is_not_whole_line() {
    let out = run_with_stdin(&["-o", "b*"], b"abb\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    // rg prints an empty line then `bb`; st skips empty hits (documented),
    // but must never print the whole source line.
    assert_eq!(stdout_text(&out), "bb\n");
}

/// Listing modes (-L) and empty inverted output keep rg's silent-exit-1 rule
/// on binary streams: no `binary file matches` notice.
#[test]
fn binary_stream_listing_and_empty_invert_stay_silent() {
    let out = run_with_stdin(&["--files-without-match", "z"], b"a\0b z\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_text(&out), "");

    let out = run_with_stdin(&["-v", "z"], b"z\0z\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_text(&out), "");
}

/// `st --files <missing path>` must error like `rg --files` instead of
/// silently listing nothing.
#[test]
fn files_mode_missing_path_errors() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext");
    write_text(&repo.path().join("a.txt"), "x\n");
    build_index(repo.path(), &index);

    let out = run_repo(
        repo.path(),
        &index,
        &["--no-update", "--files", "missing.txt"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_text(&out).contains("missing.txt: No such file"),
        "stderr:\n{}",
        stderr_text(&out)
    );
}

/// Explicit real path args win over a pipe (rg rule): the search must come
/// from the index, and the piped bytes must be ignored.
#[test]
fn stdin_pipe_loses_to_explicit_path_arg() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("f.txt"), "needle-in-file\n");
    build_index(repo.path(), index.path());
    let mut child = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index.path())
        .args(["--no-update", "-l", "needle", "f.txt"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn st");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(b"needle-in-stdin\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait st");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "f.txt\n");
}

/// The Claude Code / CI shape: stdin is /dev/null (not a tty, not a pipe) and
/// no paths are given. The repo index must still be searched; reading the
/// empty stdin instead would silently return exit 1.
#[test]
fn stdin_null_does_not_hijack_repo_search() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("f.txt"), "needle-in-file\n");
    build_index(repo.path(), index.path());
    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index.path())
        .args(["--no-update", "-l", "needle"])
        .stdin(Stdio::null())
        .output()
        .expect("run st");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "f.txt\n");
}

// ---------------------------------------------------------------------------
// pattern-vs-subcommand collision hint
// ---------------------------------------------------------------------------

#[test]
fn collision_hint_after_unknown_argument_error() {
    let out = run(&["-F", "index", "-n"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_text(&out);
    assert!(
        stderr.contains("unexpected argument"),
        "expected clap error, got:\n{stderr}"
    );
    assert!(
        stderr.contains("matched the 'index' subcommand"),
        "expected collision hint, got:\n{stderr}"
    );
    assert!(
        stderr.contains("st -e index"),
        "hint should name the -e escape hatch"
    );
}

#[test]
fn no_collision_hint_for_intended_subcommand() {
    let out = run(&["index", "--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!stderr_text(&out).contains("matched the"));
}

// ---------------------------------------------------------------------------
// --exclude-dir
// ---------------------------------------------------------------------------

#[test]
fn exclude_dir_drops_matching_directory_results() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("keep.rs"), "needle\n");
    write_text(&repo.path().join("node_modules/x/f.js"), "needle\n");
    write_text(&repo.path().join("src/node_modules/g.js"), "needle\n");
    build_index(repo.path(), index.path());
    let out = run_repo(
        repo.path(),
        index.path(),
        &[
            "--no-update",
            "--exclude-dir",
            "node_modules",
            "-l",
            "needle",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "keep.rs\n");
}

// ---------------------------------------------------------------------------
// rg-parity regression tests for the code-review fix pass (2026-08-26).
// ---------------------------------------------------------------------------

#[test]
fn pattern_file_crlf_lines_strip_trailing_cr_like_rg() {
    // rg reads CRLF pattern files as CRLF-terminated lines; keeping the `\r`
    // made every pattern from such a file silently unmatchable.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_bytes(&repo.path().join("f.txt"), b"needle here\r\nplain\r\n");
    write_bytes(&repo.path().join("pats.txt"), b"needle\r\n");
    build_index(repo.path(), index.path());
    // -f resolves against CWD, not --repo-root; pass the absolute path.
    let pats = repo.path().join("pats.txt");
    let pats = pats.to_str().unwrap();
    let out = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "-f", pats, "f.txt"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "needle here\r\n");
}

#[test]
fn stdin_pipe_empty_pattern_filters_stream_like_rg() {
    // `cmd | rg ''` prints every line; st used to hijack the pipe into a
    // whole-index search.
    let out = run_with_stdin(&[""], b"x\ny\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "x\ny\n");
}

#[test]
fn stdin_pipe_blank_pattern_file_filters_stream_like_rg() {
    let dir = tempfile::TempDir::new().unwrap();
    write_bytes(&dir.path().join("blank.pats"), b"\n");
    let out = st()
        .current_dir(dir.path())
        .args(["--no-update", "-f", "blank.pats"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .expect("piped stdin")
                .write_all(b"x\ny\n")?;
            child.wait_with_output()
        })
        .expect("run st");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "x\ny\n");
}

#[test]
#[cfg(unix)]
fn files_without_match_stdin_dash_lists_stream_when_unmatched() {
    // rg prints `<stdin>` (exit 0) when the stream itself does not match.
    let out = run_with_stdin(&["--files-without-match", "zzz", "-"], b"alpha\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "<stdin>\n");
    let out = run_with_stdin(&["--files-without-match", "alpha", "-"], b"alpha\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_text(&out), "");
}

#[test]
#[cfg(unix)]
fn files_without_match_implicit_pipe_reads_stream_not_index() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("f.txt"), "needle\n");
    build_index(repo.path(), index.path());
    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index.path())
        .args(["--no-update", "--files-without-match", "zzz"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .expect("piped stdin")
                .write_all(b"alpha\n")?;
            child.wait_with_output()
        })
        .expect("run st");
    // The stream is the only input: it does not match, so rg lists it.
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "<stdin>\n");
}

#[test]
#[cfg(unix)]
fn mixed_dash_and_path_label_both_halves() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("a.txt"), "zebra\n");
    build_index(repo.path(), index.path());
    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index.path())
        .args(["--no-update", "-n", "zebra", "a.txt", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .expect("piped stdin")
                .write_all(b"zebra\n")?;
            child.wait_with_output()
        })
        .expect("run st");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let stdout = stdout_text(&out);
    assert!(
        stdout.contains("a.txt:1:zebra\n"),
        "file half must carry its path prefix, got: {stdout:?}"
    );
    assert!(
        stdout.contains("<stdin>:1:zebra\n"),
        "stdin half must carry the <stdin> prefix, got: {stdout:?}"
    );
}

#[test]
fn mixed_dash_and_whole_repo_path_labels_both_halves_without_h() {
    // Regression: `explicit_path_specs` drops a spec whose `rel_path`
    // normalizes to empty (e.g. "."), so `-` + "." previously collapsed to
    // a single (`-`'s) spec and `shows_filename_by_default` misread this
    // genuinely 2-input search as "one plain file", suppressing the
    // filename prefix rg shows on both halves. Deliberately does not pass
    // `-H`: the sibling `mixed_dash_and_path_label_both_halves` test uses a
    // concrete filename (whose spec never collapses to empty) so it never
    // exercised this auto-detect path.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("a.txt"), "zebra\n");
    build_index(repo.path(), index.path());
    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(index.path())
        .args(["--no-update", "-n", "zebra", "-", "."])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .expect("piped stdin")
                .write_all(b"zebra\n")?;
            child.wait_with_output()
        })
        .expect("run st");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let stdout = stdout_text(&out);
    assert!(
        stdout.contains("a.txt:1:zebra\n"),
        "file half must carry its path prefix, got: {stdout:?}"
    );
    assert!(
        stdout.contains("<stdin>:1:zebra\n"),
        "stdin half must carry the <stdin> prefix, got: {stdout:?}"
    );
}

#[test]
#[cfg(unix)]
fn stdin_invert_only_matching_prints_whole_lines_like_rg() {
    let out = run_with_stdin(&["-v", "-o", "zzq"], b"a\nb\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "a\nb\n");
    let out = run_with_stdin(&["-v", "--vimgrep", "zzq"], b"a\nb\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "<stdin>:1:a\n<stdin>:2:b\n");
}

#[test]
#[cfg(unix)]
fn stdin_only_matching_byte_offset_is_last_prefix_field() {
    // rg prints [line:]byte:match; st used to print byte first.
    let out = run_with_stdin(&["-n", "-b", "-o", "b"], b"ab\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "1:1:b\n");
}

#[test]
#[cfg(unix)]
fn stdin_json_binary_offset_reports_first_nul() {
    let out = run_with_stdin(&["--json", "parse"], b"a\0b parse\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert!(
        stdout_text(&out).contains("\"binary_offset\":1"),
        "end event must report the first NUL offset like rg"
    );
}

#[test]
#[cfg(unix)]
fn heading_single_stdin_input_prints_no_heading() {
    let out = run_with_stdin(&["--heading", "foo"], b"foo bar\n");
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "foo bar\n");
}

#[test]
fn indexed_invert_match_byte_offsets_print_like_rg() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_bytes(&repo.path().join("f.txt"), b"foo\r\nbar baz\n");
    build_index(repo.path(), index.path());
    let out = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "-v", "-b", "zzq", "f.txt"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "0:foo\r\n5:bar baz\n");
}

#[test]
fn indexed_json_excludes_cr_submatches_on_crlf_lines() {
    // rg's oracle reference always runs with `--crlf` (tests/integration/
    // oracle_helpers.rs), which folds a bare trailing `\r` into the line
    // terminator: verified directly against rg 15.2.0 --crlf, `\s` on
    // "plain\r\n" reports zero submatches (the `\r` is never searchable
    // content), not a `\r` submatch. `st`'s JSON submatches must match that.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_bytes(&repo.path().join("f.txt"), b"plain\r\n");
    build_index(repo.path(), index.path());
    let out = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "--json", "\\s", "f.txt"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    assert!(
        !stdout_text(&out).contains("\"text\":\"\\r\""),
        "json submatches must not report a \\r match; \\r is terminator, not content"
    );
    assert!(
        stdout_text(&out).contains("\"submatches\":[]"),
        "no whitespace besides the terminator \\r exists in \"plain\""
    );
}

#[test]
fn indexed_json_no_phantom_submatch_after_unterminated_final_line() {
    // Regression (oracle fixture repro_45977b47dc1f41aa.json / re-minimized
    // repro_8ffb628246265813.json): a zero-width match at the very end of the
    // file's last line, when that line has no trailing `\n` at all, is a
    // phantom rg never reports (no terminator exists there for its
    // line-oriented searcher to anchor past). This hit two distinct code
    // paths: an `-x` CRLF-anchor match on a final line ending in a bare `\r`,
    // and a bare-`|` pattern's per-byte empty-match enumeration on any
    // unterminated final line.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_bytes(&repo.path().join("f.txt"), b"query\nparse\r");
    build_index(repo.path(), index.path());
    let out = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "--json", "-x", "parse|", "f.txt"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let text = stdout_text(&out);
    assert_eq!(
        text.matches("\"type\":\"match\"").count(),
        1,
        "only the \"parse\\r\" line should match: {text}"
    );
    assert!(
        text.contains("\"submatches\":[{\"end\":5,\"match\":{\"text\":\"parse\"},\"start\":0}]"),
        "must report exactly one submatch (\"parse\"), no trailing phantom: {text}"
    );
}

#[test]
fn missing_explicit_path_exits_2_like_rg() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("f.txt"), "needle\n");
    build_index(repo.path(), index.path());
    let out = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "zzq", "no_such_file.txt"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_text(&out).contains("no_such_file.txt"),
        "error must name the missing path"
    );
}

#[test]
fn missing_explicit_path_searches_nothing_not_whole_repo() {
    // Regression: when every explicitly named path is missing, `st` must
    // report the IO error and match nothing else, exactly like `rg`. A prior
    // version stripped missing paths from the search scope, which emptied
    // the path list entirely and made `explicit_path_specs` fall back to its
    // "no paths given" case -- silently searching (and leaking matches from)
    // the whole repo instead of the nothing rg reports.
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("unrelated.txt"), "needle here\n");
    build_index(repo.path(), index.path());

    let out = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "needle", "no_such_file.txt"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr_text(&out).contains("no_such_file.txt"),
        "error must name the missing path; stderr:\n{}",
        stderr_text(&out)
    );
    assert_eq!(
        stdout_text(&out),
        "",
        "a missing explicit path must not fall back to a whole-repo search; stdout:\n{}",
        stdout_text(&out)
    );
}

#[test]
fn max_results_caps_total_output_and_prints_a_notice() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    for name in ["src/a.rs", "src/b.rs", "src/c.rs"] {
        write_text(
            &repo.path().join(name),
            "shared_cap_token one\nshared_cap_token two\n",
        );
    }
    build_index(repo.path(), index.path());

    // 6 matches across 3 files, capped at 2 lines.
    let output = run_repo(repo.path(), index.path(), &["--max-results", "2", "-n", "shared_cap_token"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_text(&output).lines().count(),
        2,
        "stdout:\n{}",
        stdout_text(&output)
    );
    assert!(
        stderr_text(&output).contains("output truncated at 2 match(es) (--max-results)"),
        "stderr:\n{}",
        stderr_text(&output)
    );

    // Under -l the unit is files, so a cap of 3 covers all three.
    let listed = run_repo(repo.path(), index.path(), &["--max-results", "3", "-l", "shared_cap_token"]);
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout_text(&listed).lines().count(), 3);
    assert!(
        !stderr_text(&listed).contains("truncated"),
        "3 files under a cap of 3 is not truncation\nstderr:\n{}",
        stderr_text(&listed)
    );

    let listed_cut = run_repo(repo.path(), index.path(), &["--max-results", "2", "-l", "shared_cap_token"]);
    assert_eq!(stdout_text(&listed_cut).lines().count(), 2);
    assert!(stderr_text(&listed_cut).contains("2 file(s)"));

    // -q suppresses the notice along with the output.
    let quiet = run_repo(repo.path(), index.path(), &["--max-results", "1", "-q", "shared_cap_token"]);
    assert_eq!(quiet.status.code(), Some(0));
    assert!(stderr_text(&quiet).is_empty(), "stderr:\n{}", stderr_text(&quiet));
}

#[test]
fn max_results_json_summary_carries_truncated_only_when_the_flag_is_passed() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(
        &repo.path().join("src/a.rs"),
        "json_cap_token one\njson_cap_token two\njson_cap_token three\n",
    );
    build_index(repo.path(), index.path());

    let capped = run_repo(repo.path(), index.path(), &["--json", "--max-results", "2", "json_cap_token"]);
    let last = stdout_text(&capped)
        .lines()
        .last()
        .expect("summary line")
        .to_string();
    let summary: serde_json::Value = serde_json::from_str(&last).unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["data"]["truncated"], serde_json::json!(true));

    let exact = run_repo(repo.path(), index.path(), &["--json", "--max-results", "3", "json_cap_token"]);
    let last = stdout_text(&exact).lines().last().unwrap().to_string();
    let summary: serde_json::Value = serde_json::from_str(&last).unwrap();
    assert_eq!(summary["data"]["truncated"], serde_json::json!(false));

    // Without the flag the summary keeps rg parity: no extra key at all.
    let plain = run_repo(repo.path(), index.path(), &["--json", "json_cap_token"]);
    let last = stdout_text(&plain).lines().last().unwrap().to_string();
    let summary: serde_json::Value = serde_json::from_str(&last).unwrap();
    assert!(
        summary["data"].get("truncated").is_none(),
        "summary:\n{last}"
    );
}

#[test]
fn max_results_is_refused_by_the_modes_it_cannot_cap() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    write_text(&repo.path().join("src/a.rs"), "refuse_cap_token\n");
    build_index(repo.path(), index.path());

    for mode in [
        vec!["-c"],
        vec!["--count-matches"],
        vec!["-v"],
        vec!["--files-without-match"],
    ] {
        let mut args = vec!["--max-results", "1"];
        args.extend(mode.iter().copied());
        args.push("refuse_cap_token");
        let output = run_repo(repo.path(), index.path(), &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{mode:?} should refuse --max-results\nstderr:\n{}",
            stderr_text(&output)
        );
        assert!(stderr_text(&output).contains("--max-results is not supported with"));
    }

    let files = run_repo(repo.path(), index.path(), &["--files", "--max-results", "1"]);
    assert_eq!(files.status.code(), Some(2));
    assert!(stderr_text(&files).contains("--max-results is not supported with --files"));
}

/// `st update` must persist uncommitted drift, not just apply it to its own
/// overlay. The proof is a *separate process* that is forbidden from updating:
/// if `st --no-update` finds the content, it came off disk.
#[test]
fn st_update_persists_uncommitted_drift_for_a_later_process() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    };
    git(&["init"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "user.email", "test@test"]);
    write_text(&repo.path().join("src/a.rs"), "fn committed_only() {}\n");
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial", "--no-gpg-sign"]);
    build_index(repo.path(), index.path());

    // Uncommitted edit. Age the mtime so the worktree anchor's racy-mtime rule
    // trusts it; without that the path is deliberately re-applied next pass and
    // the files_behind assertion below would be measuring the wrong thing.
    write_text(&repo.path().join("src/a.rs"), "fn uncommitted_drift() {}\n");
    std::fs::File::options()
        .write(true)
        .open(repo.path().join("src/a.rs"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(10))
        .unwrap();

    let updated = run_repo(repo.path(), index.path(), &["update", "--quiet"]);
    assert_eq!(updated.status.code(), Some(0), "{}", stderr_text(&updated));

    // The definitive assertion: a brand-new process, with updating switched
    // off, still sees the edit.
    let found = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "-q", "uncommitted_drift"],
    );
    assert_eq!(
        found.status.code(),
        Some(0),
        "a later process must see the flushed edit\nstderr:\n{}",
        stderr_text(&found)
    );

    let gone = run_repo(
        repo.path(),
        index.path(),
        &["--no-update", "-q", "committed_only"],
    );
    assert_eq!(gone.status.code(), Some(1), "superseded content must be gone");

    // And the flushed path is no longer counted as outstanding work.
    let status = run_repo(repo.path(), index.path(), &["status", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&stdout_text(&status)).unwrap();
    assert_eq!(
        parsed["files_behind"],
        serde_json::json!(0),
        "status:\n{}",
        stdout_text(&status)
    );

    // `--flush` is still accepted, and still exits 0.
    let flush = run_repo(repo.path(), index.path(), &["update", "--flush", "--quiet"]);
    assert_eq!(flush.status.code(), Some(0), "{}", stderr_text(&flush));
}
