use super::*;

#[cfg(unix)]
mod tests {
    use super::*;

    #[test]
    fn relativize_resolves_relative_path_against_cwd() {
        // Relative CLI paths resolve against CWD (rg semantics), not the repo
        // root. `st pat src/` from crates/foo scopes to crates/foo/src.
        let repo_root = Path::new("/repo");
        let cwd = Path::new("/repo/crates/foo");
        assert_eq!(
            relativize_cli_path(repo_root, cwd, Path::new("src")),
            PathBuf::from("crates/foo/src")
        );
    }

    #[test]
    fn relativize_dot_resolves_to_cwd_subdir_not_empty() {
        // "." from a subdir must produce the subdir's repo-relative path, not
        // an empty spec that gets filtered out and silently searches the repo.
        let repo_root = Path::new("/repo");
        let cwd = Path::new("/repo/crates/foo");
        assert_eq!(
            relativize_cli_path(repo_root, cwd, Path::new(".")),
            PathBuf::from("crates/foo")
        );
    }

    #[test]
    fn relativize_absolute_path_strips_repo_root() {
        let repo_root = Path::new("/repo");
        let cwd = Path::new("/repo/crates/foo");
        assert_eq!(
            relativize_cli_path(repo_root, cwd, Path::new("/repo/src/lib.rs")),
            PathBuf::from("src/lib.rs")
        );
    }

    #[test]
    fn relativize_falls_back_to_repo_root_when_cwd_outside_repo() {
        // Explicit --repo-root pointing at a repo the caller is not standing in:
        // relative paths stay repo-root-relative so they still reach the index.
        let repo_root = Path::new("/repo");
        let cwd = Path::new("/elsewhere");
        assert_eq!(
            relativize_cli_path(repo_root, cwd, Path::new("src/one.rs")),
            PathBuf::from("src/one.rs")
        );
    }
}

/// Tests for `matches_optional_glob` (glob semantics) and `path_depth`
/// (max-depth logic). Pure logic tests, no filesystem access.
mod glob_and_depth_tests {
    use super::*;

    // --- matches_optional_glob: globset-backed behavior ---

    #[test]
    fn glob_star_extension_passes_deep_path() {
        // *.rs has no '/' → matched against basename → must match src/main.rs.
        let path = Path::new("src/main.rs");
        assert!(matches_optional_glob(path, &[], &[], &["*.rs".to_string()]));
    }

    #[test]
    fn glob_star_extension_rejects_wrong_ext() {
        let path = Path::new("src/main.py");
        assert!(!matches_optional_glob(path, &[], &[], &["*.rs".to_string()]));
    }

    #[test]
    fn glob_negation_excludes_vendor() {
        // !vendor/** has '/' → matched against full path.
        let vendor = Path::new("vendor/lib.rs");
        let src = Path::new("src/lib.rs");
        let globs = vec!["!vendor/**".to_string()];
        assert!(!matches_optional_glob(vendor, &[], &[], &globs));
        assert!(matches_optional_glob(src, &[], &[], &globs));
    }

    #[test]
    fn glob_character_class_on_basename() {
        // [abcde]file.rs has no '/' → matched against basename.
        let path_a = Path::new("foo/afile.rs");
        let path_z = Path::new("foo/zfile.rs");
        let globs = vec!["[abcde]file.rs".to_string()];
        assert!(matches_optional_glob(path_a, &[], &[], &globs));
        assert!(!matches_optional_glob(path_z, &[], &[], &globs));
    }

    #[test]
    fn glob_character_class_in_path_pattern() {
        // When the pattern has '/' it's a path pattern with literal_separator.
        let path_a = Path::new("foo/afile.rs");
        let path_z = Path::new("foo/zfile.rs");
        let globs = vec!["**/[abcde]file.rs".to_string()];
        assert!(matches_optional_glob(path_a, &[], &[], &globs));
        assert!(!matches_optional_glob(path_z, &[], &[], &globs));
    }

    #[test]
    fn glob_alternation_on_basename() {
        // *.{rs,py} has no '/' → basename match.
        let rs_path = Path::new("src/lib.rs");
        let py_path = Path::new("src/lib.py");
        let js_path = Path::new("src/lib.js");
        let globs = vec!["*.{rs,py}".to_string()];
        assert!(matches_optional_glob(rs_path, &[], &[], &globs));
        assert!(matches_optional_glob(py_path, &[], &[], &globs));
        assert!(!matches_optional_glob(js_path, &[], &[], &globs));
    }

    #[test]
    fn glob_alternation_in_path_pattern() {
        // **/*.{rs,py} has '/' → full path match.
        let rs_path = Path::new("src/lib.rs");
        let py_path = Path::new("src/lib.py");
        let js_path = Path::new("src/lib.js");
        let globs = vec!["**/*.{rs,py}".to_string()];
        assert!(matches_optional_glob(rs_path, &[], &[], &globs));
        assert!(matches_optional_glob(py_path, &[], &[], &globs));
        assert!(!matches_optional_glob(js_path, &[], &[], &globs));
    }

    #[test]
    fn glob_literal_separator_prevents_substring_match() {
        // "src/foo/**" has '/' → literal_separator(true) prevents
        // "mysrc/foo" from matching because 'src' is not a complete component.
        let bad = Path::new("mysrc/foo/bar.rs");
        let good = Path::new("src/foo/bar.rs");
        let globs = vec!["src/foo/**".to_string()];
        assert!(!matches_optional_glob(bad, &[], &[], &globs));
        assert!(matches_optional_glob(good, &[], &[], &globs));
    }

    #[test]
    fn glob_empty_returns_true() {
        assert!(matches_optional_glob(Path::new("anything"), &[], &[], &[]));
    }

    // --- path_depth ---

    #[test]
    fn path_depth_counts_components_minus_one() {
        assert_eq!(path_depth(Path::new("file.rs")), 0);
        assert_eq!(path_depth(Path::new("src/file.rs")), 1);
        assert_eq!(path_depth(Path::new("src/a/b/file.rs")), 3);
    }
}
