use super::*;

fn make_index() -> PathIndex {
    let paths = vec![
        std::path::PathBuf::from("src/main.rs"),
        std::path::PathBuf::from("src/lib.rs"),
        std::path::PathBuf::from("src/util.py"),
        std::path::PathBuf::from("tests/test_main.rs"),
        std::path::PathBuf::from("docs/readme.md"),
        std::path::PathBuf::from("scripts/build.js"),
    ];
    PathIndex::build(&paths)
}

#[test]
fn filter_by_extension() {
    let idx = make_index();
    let filter = build_filter(&idx, Some("rs"), None, None, None).unwrap();
    assert_eq!(filter.file_ids.len(), 3);
}

#[test]
fn filter_by_path_glob() {
    let idx = make_index();
    let filter = build_filter(&idx, None, None, Some("src/"), None).unwrap();
    assert_eq!(filter.file_ids.len(), 3);
}

#[test]
fn filter_combined_type_and_path() {
    let idx = make_index();
    let filter = build_filter(&idx, Some("rs"), None, Some("src/"), None).unwrap();
    assert_eq!(filter.file_ids.len(), 2);
}

#[test]
fn filter_exclude_type() {
    let idx = make_index();
    let filter = build_filter(&idx, None, Some("js"), None, None).unwrap();
    assert_eq!(filter.file_ids.len(), 5);
}

#[test]
fn no_filter_returns_none() {
    let idx = make_index();
    let filter = build_filter(&idx, None, None, None, None);
    assert!(filter.is_none());
}

#[test]
fn glob_star_extension() {
    assert!(path_matches_glob(Path::new("src/main.rs"), "*.rs"));
    assert!(!path_matches_glob(Path::new("src/main.py"), "*.rs"));
}

#[test]
fn glob_double_star_extension() {
    assert!(path_matches_glob(
        Path::new("deep/nested/file.rs"),
        "**/*.rs"
    ));
    assert!(!path_matches_glob(
        Path::new("deep/nested/file.py"),
        "**/*.rs"
    ));
}

#[test]
fn matches_path_filter_combines_type_and_glob() {
    assert!(matches_path_filter(
        Path::new("src/main.rs"),
        Some("rs"),
        None,
        Some("src/")
    ));
    assert!(!matches_path_filter(
        Path::new("src/main.py"),
        Some("rs"),
        None,
        Some("src/")
    ));
    assert!(!matches_path_filter(
        Path::new("tests/main.rs"),
        Some("rs"),
        None,
        Some("src/")
    ));
}

#[test]
fn bare_word_glob_requires_component_boundary() {
    assert!(path_matches_glob(Path::new("test/foo.rs"), "test"));
    assert!(path_matches_glob(Path::new("src/test.rs"), "test"));
    assert!(path_matches_glob(Path::new("src/test/util.rs"), "test"));
    assert!(!path_matches_glob(Path::new("src/contest.rs"), "test"));
    assert!(!path_matches_glob(Path::new("src/testing.rs"), "test"));
}

#[test]
fn path_with_slash_still_uses_substring() {
    assert!(path_matches_glob(Path::new("src/test/foo.rs"), "src/test"));
    assert!(!path_matches_glob(Path::new("lib/test/foo.rs"), "src/test"));
}

#[test]
fn wildcard_glob_matches_file_component() {
    assert!(path_matches_glob(
        Path::new("tests/search_tests.rs"),
        "*tests.rs"
    ));
    assert!(!path_matches_glob(
        Path::new("tests/search.rs"),
        "*tests.rs"
    ));
}

#[test]
fn wildcard_glob_with_slash_matches_component_suffix() {
    assert!(path_matches_glob(Path::new("vendor/lib.rs"), "vendor/**"));
    assert!(path_matches_glob(
        Path::new("src/vendor/lib.rs"),
        "vendor/**"
    ));
    assert!(!path_matches_glob(
        Path::new("src/not_vendor/lib.rs"),
        "vendor/**"
    ));
}

#[test]
fn double_star_slash_bare_word_requires_component_boundary() {
    assert!(
        !path_matches_glob(Path::new("src/contest.rs"), "**/test"),
        "**/test must not match 'contest.rs' (substring, not component)"
    );
    assert!(
        !path_matches_glob(Path::new("src/testing.rs"), "**/test"),
        "**/test must not match 'testing.rs'"
    );
    assert!(
        path_matches_glob(Path::new("test/foo.rs"), "**/test"),
        "**/test must match 'test/foo.rs'"
    );
    assert!(
        path_matches_glob(Path::new("src/test.rs"), "**/test"),
        "**/test must match 'src/test.rs' (stem matches component)"
    );
    assert!(
        path_matches_glob(Path::new("src/test/util.rs"), "**/test"),
        "**/test must match when test is a directory component"
    );
}

#[test]
fn double_star_slash_with_slash_still_uses_substring() {
    assert!(path_matches_glob(
        Path::new("deep/src/test/util.rs"),
        "**/src/test"
    ));
    assert!(!path_matches_glob(
        Path::new("deep/lib/test/util.rs"),
        "**/src/test"
    ));
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_participate_in_extension_and_glob_filters() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let path = std::path::PathBuf::from(OsString::from_vec(b"src/odd\xff.rs".to_vec()));
    assert!(matches_path_filter(&path, Some("rs"), None, Some("src/")));
    assert!(path_matches_glob(&path, "*.rs"));
    assert!(path_matches_glob(&path, "src/"));
}

#[test]
fn byte_split_ext_no_sep() {
    let s: &[u8] = b"nodot";
    assert_eq!(ByteSplitExt::rsplit_once(s, |&b| b == b'.'), None);
}

#[test]
fn byte_split_ext_last_sep() {
    let s: &[u8] = b"foo.bar.baz";
    let (head, tail) = ByteSplitExt::rsplit_once(s, |&b| b == b'.').unwrap();
    assert_eq!(head, b"foo.bar");
    assert_eq!(tail, b"baz");
}

#[test]
fn filter_uses_glob_cache() {
    let idx = make_index();
    let cache = std::sync::Mutex::new(std::collections::HashMap::new());
    let filter1 = build_filter(&idx, None, None, Some("src/"), Some(&cache)).unwrap();
    assert_eq!(filter1.file_ids.len(), 3);
    {
        let guard = cache.lock().unwrap();
        assert!(guard.contains_key("src/"));
        assert_eq!(guard.get("src/").unwrap().len(), 3);
    }
    let filter2 = build_filter(&idx, None, None, Some("src/"), Some(&cache)).unwrap();
    assert_eq!(filter2.file_ids.len(), 3);
}
