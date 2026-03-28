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
    st()
        .arg("--repo-root")
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
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let chunk = ((bytes[i] as u32) << 16)
            | ((bytes[i + 1] as u32) << 8)
            | (bytes[i + 2] as u32);
        out.push(TABLE[((chunk >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((chunk >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((chunk >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(chunk & 0x3F) as usize] as char);
        i += 3;
    }

    match bytes.len() - i {
        1 => {
            let chunk = (bytes[i] as u32) << 16;
            out.push(TABLE[((chunk >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((chunk >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let chunk = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
            out.push(TABLE[((chunk >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((chunk >> 12) & 0x3F) as usize] as char);
            out.push(TABLE[((chunk >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }

    out
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
    write_text(&repo.path().join("src/lib.rs"), "fn needle() {}\nfn helper() {}\n");
    build_index(repo.path(), index.path());

    let hit = run_repo(repo.path(), index.path(), &["needle"]);
    assert_eq!(hit.status.code(), Some(0));
    assert!(stdout_text(&hit).contains("needle"));

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
    write_text(&repo.path().join("src/main.rs"), "fn main() { println!(\"x\"); }\n");
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["documents"].as_u64().unwrap() >= 1);
    assert!(value["segments"].as_u64().unwrap() >= 1);
    assert_eq!(value["index_dir"], index.path().display().to_string());
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
        .map(|msg| msg["data"]["path"]["text"].as_str().unwrap())
        .collect();
    assert!(matched_paths.contains(&"src/one.rs"));
    assert!(matched_paths.contains(&"src/two.rs"));
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
    assert_eq!(stdout_text(&files), "src/sample.rs\n");

    let counts = run_repo(repo.path(), index.path(), &["-c", "needle"]);
    assert_eq!(counts.status.code(), Some(0));
    assert_eq!(stdout_text(&counts), "src/sample.rs:2\n");

    let heading = run_repo(repo.path(), index.path(), &["--heading", "needle"]);
    assert_eq!(heading.status.code(), Some(0));
    assert!(stdout_text(&heading).starts_with("src/sample.rs\n"));

    let context = run_repo(repo.path(), index.path(), &["-C", "1", "needle"]);
    assert_eq!(context.status.code(), Some(0));
    let text = stdout_text(&context);
    assert!(text.contains("src/sample.rs:2:needle on line 2"));
    assert!(text.contains("src/sample.rs-3-line 3"));
    assert!(text.contains("src/sample.rs:6:needle on line 6"));
    assert!(text.contains("--\n"));
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
    assert!(stdout_text(&text_hit).contains("src/text.rs"));

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

    let expected = b"src/non_utf8.txt:1:prefix\xFFneedle\x80suffix\n";

    let literal = run_repo(repo.path(), index.path(), &["-F", "needle"]);
    assert_eq!(literal.status.code(), Some(0));
    assert_eq!(literal.stdout, expected);

    let regex = run_repo(repo.path(), index.path(), &["(?-u)\\xFFneedle\\x80"]);
    assert_eq!(regex.status.code(), Some(0));
    assert_eq!(regex.stdout, expected);
}

#[test]
fn json_output_uses_bytes_fields_for_non_utf8_match_lines() {
    let repo = tempfile::TempDir::new().unwrap();
    let index = tempfile::TempDir::new().unwrap();
    let line = b"prefix\xFFneedle\x80suffix\n";
    write_bytes(&repo.path().join("src/non_utf8.txt"), line);
    build_index(repo.path(), index.path());

    let output = run_repo(repo.path(), index.path(), &["--json", "(?-u)\\xFFneedle\\x80"]);
    assert_eq!(output.status.code(), Some(0));

    let messages: Vec<serde_json::Value> = stdout_text(&output)
        .lines()
        .map(|entry| serde_json::from_str(entry).expect("valid NDJSON line"))
        .collect();

    let matched = messages
        .iter()
        .find(|msg| msg["type"] == "match")
        .expect("match message");

    assert_eq!(matched["data"]["path"]["text"], "src/non_utf8.txt");
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
    assert_eq!(output.stdout, b"src/odd\xff.rs:1:needle\n");
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
    assert_eq!(output.stdout, b"src/odd\xff.rs:1:needle\n");
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
