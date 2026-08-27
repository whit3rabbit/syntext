//! Unit tests for the rg/grep fallback argv translation.

use super::*;

fn osv(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn to_strings(items: Vec<OsString>) -> Vec<String> {
    items
        .into_iter()
        .map(|o| o.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn filter_strips_bool_flags() {
    let got = filter_st_args(osv(&["st", "--verbose", "foo", "--fallback", "src"]));
    assert_eq!(to_strings(got), vec!["foo", "src"]);
}

#[test]
fn filter_strips_value_flags_separate_form() {
    let got = filter_st_args(osv(&[
        "st",
        "--repo-root",
        "/tmp/r",
        "foo",
        "--index-dir",
        "/tmp/i",
        "src",
    ]));
    assert_eq!(to_strings(got), vec!["foo", "src"]);
}

#[test]
fn filter_strips_value_flags_eq_form() {
    let got = filter_st_args(osv(&["st", "--repo-root=/tmp/r", "--index=/tmp/i", "foo"]));
    assert_eq!(to_strings(got), vec!["foo"]);
}

#[test]
fn filter_preserves_rg_shared_flags() {
    let got = filter_st_args(osv(&["st", "-i", "--json", "-A", "2", "-e", "foo", "src"]));
    assert_eq!(
        to_strings(got),
        vec!["-i", "--json", "-A", "2", "-e", "foo", "src"]
    );
}

#[test]
fn filter_translates_rust_flag_to_rg_type() {
    // rg's type table has no "rs" (it rejects `-t rs`), so st's extension
    // spelling must become rg's type name.
    let got = filter_st_args(osv(&["st", "--rust", "foo", "src"]));
    assert_eq!(to_strings(got), vec!["-t", "rust", "foo", "src"]);
    let got = filter_st_args(osv(&["st", "--rs", "foo"]));
    assert_eq!(to_strings(got), vec!["-t", "rust", "foo"]);
}

#[test]
fn filter_translates_exclude_dir_to_globs() {
    let got = filter_st_args(osv(&["st", "--exclude-dir", "node_modules", "foo", "."]));
    assert_eq!(
        to_strings(got),
        vec!["-g", "!node_modules/**", "-g", "!**/node_modules/**", "foo", "."]
    );
    let got = filter_st_args(osv(&["st", "--exclude-dir=target", "foo"]));
    assert_eq!(
        to_strings(got),
        vec!["-g", "!target/**", "-g", "!**/target/**", "foo"]
    );
}

#[test]
fn grep_args_map_common_flags() {
    let args = SearchArgs {
        pattern: "needle".to_string(),
        paths: vec![PathBuf::from("src")],
        ignore_case: true,
        word_regexp: true,
        after_context: 2,
        ..SearchArgs::default()
    };
    let got = to_strings(build_grep_args(&args));
    assert!(got.contains(&"-r".to_string()));
    assert!(got.contains(&"-n".to_string()));
    assert!(got.contains(&"-E".to_string()));
    assert!(got.contains(&"-i".to_string()));
    assert!(got.contains(&"-w".to_string()));
    assert_eq!(
        got.windows(2).find(|w| w[0] == "-A"),
        Some(["-A".to_string(), "2".to_string()].as_slice())
    );
    // pattern is passed via -e, paths trail.
    assert_eq!(
        got.windows(2).find(|w| w[0] == "-e"),
        Some(["-e".to_string(), "needle".to_string()].as_slice())
    );
    assert_eq!(got.last().unwrap(), "src");
}

#[test]
fn grep_args_default_paths_to_dot_and_fixed_strings() {
    let args = SearchArgs {
        pattern: "lit".to_string(),
        fixed_strings: true,
        ..SearchArgs::default()
    };
    let got = to_strings(build_grep_args(&args));
    assert!(got.contains(&"-F".to_string()));
    assert!(!got.contains(&"-E".to_string()));
    assert_eq!(got.last().unwrap(), ".");
}

#[test]
fn grep_args_map_globs_to_include_exclude() {
    let args = SearchArgs {
        pattern: "x".to_string(),
        globs: vec!["*.rs".to_string(), "!*.lock".to_string()],
        ..SearchArgs::default()
    };
    let got = to_strings(build_grep_args(&args));
    assert!(got.contains(&"--include=*.rs".to_string()));
    assert!(got.contains(&"--exclude=*.lock".to_string()));
}

#[test]
fn grep_args_map_exclude_dirs_natively() {
    // The derived `!D/**` globs are basename no-ops under grep; the native
    // flag is the faithful mapping.
    let args = SearchArgs {
        pattern: "x".to_string(),
        exclude_dirs: vec!["node_modules".to_string()],
        ..SearchArgs::default()
    };
    let got = to_strings(build_grep_args(&args));
    assert!(got.contains(&"--exclude-dir=node_modules".to_string()));
}
