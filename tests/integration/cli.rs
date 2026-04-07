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
