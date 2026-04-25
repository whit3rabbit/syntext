use std::fs;
use std::path::Path;

use super::rewrite::rewrite_for_cwd;

fn with_indexed_repo(test: impl FnOnce(&Path)) {
    let dir = tempfile::TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".syntext")).unwrap();
    test(dir.path());
}

fn rewrite(command: &str, cwd: &Path) -> Option<String> {
    rewrite_for_cwd(command, cwd, "st")
}

#[test]
fn rg_baseline_rewrites_when_index_exists() {
    with_indexed_repo(|cwd| {
        assert_eq!(
            rewrite(r#"rg "parse_query" src/"#, cwd).as_deref(),
            Some("st parse_query src/")
        );
        assert_eq!(
            rewrite(r#"rg -n -i "todo" ."#, cwd).as_deref(),
            Some("st -n -i todo .")
        );
    });
}

#[test]
fn rg_supported_type_glob_regexp_and_json_rewrite() {
    with_indexed_repo(|cwd| {
        assert_eq!(
            rewrite(r#"rg --json -t rs -g "*.rs" -e "parse_query" src/"#, cwd).as_deref(),
            Some("st --json -t rs -g '*.rs' -e parse_query src/")
        );
    });
}

#[test]
fn grep_recursive_forms_rewrite_and_strip_grep_only_flags() {
    with_indexed_repo(|cwd| {
        assert_eq!(
            rewrite(r#"grep -RIn "unsafe" src/"#, cwd).as_deref(),
            Some("st -n unsafe src/")
        );
        assert_eq!(
            rewrite(r#"grep -Ri "unsafe" src/"#, cwd).as_deref(),
            Some("st -i unsafe src/")
        );
        assert_eq!(
            rewrite(r#"grep -RF "unsafe" src/"#, cwd).as_deref(),
            Some("st -F unsafe src/")
        );
    });
}

#[test]
fn rewrite_preserves_env_prefixes_and_control_segments() {
    with_indexed_repo(|cwd| {
        assert_eq!(
            rewrite("LC_ALL=C rg foo src/ && rg bar tests/", cwd).as_deref(),
            Some("LC_ALL=C st foo src/ && st bar tests/")
        );
    });
}

#[test]
fn no_rewrite_without_index() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(rewrite("rg foo src/", dir.path()), None);
}

#[test]
fn stdin_and_ambiguous_shell_forms_pass_through() {
    with_indexed_repo(|cwd| {
        assert_eq!(rewrite("cat file.rs | grep foo", cwd), None);
        assert_eq!(rewrite("rg foo > out.txt", cwd), None);
        assert_eq!(rewrite(r#"rg "$PATTERN" src/"#, cwd), None);
        assert_eq!(rewrite(r#"rg "unterminated"#, cwd), None);
    });
}

#[test]
fn unsupported_rg_and_grep_forms_pass_through() {
    with_indexed_repo(|cwd| {
        assert_eq!(rewrite("grep foo", cwd), None);
        assert_eq!(rewrite("grep -R foo", cwd), None);
        assert_eq!(rewrite("rg --files", cwd), None);
        assert_eq!(rewrite(r#"grep -P "lookbehind" src/"#, cwd), None);
        assert_eq!(rewrite("grep -z foo file", cwd), None);
        assert_eq!(rewrite("rg --pre 'cmd' foo", cwd), None);
    });
}
