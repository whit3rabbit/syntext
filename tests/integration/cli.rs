use std::fs;
use std::io::{BufRead, Read};
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
    syntext::base64::encode(bytes)
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
        text.lines().any(|line| line.starts_with("Behind:") && line.contains('3')),
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
        &["-F", "-g", "src/", "-t", "rs", "needle"],
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
    assert_eq!(
        fix_path(stdout_text(&output)),
        "src/one.txt\nsrc/two.txt\n"
    );
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

    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .env("SYNTEXT_FALLBACK_RG", "1")
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
fn missing_index_without_optin_errors_with_guidance() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = repo.path().join(".syntext"); // never created -> IndexNotFound

    let out = st()
        .arg("--repo-root")
        .arg(repo.path())
        .arg("--index-dir")
        .arg(&index)
        .env_remove("SYNTEXT_FALLBACK_RG")
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
        stderr.contains(
            "st: index is ~4 files behind; searching stale (run 'st update')"
        ),
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
    assert_eq!(parent_git_calls, 3, "expected only the parent's 3 detection calls");

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

    let mut run_cmd = Command::new(parts[0]);
    run_cmd.current_dir(repo.path());
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

    let run_with_path = |query: &str, path: &str| {
        st()
            .arg("--repo-root")
            .arg(repo.path())
            .arg("--index-dir")
            .arg(&index_dir)
            .env("PATH", path)
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
