//! Ripgrep correctness oracle tests.
//!
//! These tests compare ripline search results against `rg` (ripgrep) for
//! the same patterns on the same fixture corpus. ripline must produce
//! zero false negatives for all indexed patterns: every file that rg finds
//! must also appear in ripline results.
//!
//! False positives (candidates filtered by the verifier) are acceptable
//! at the index level but must not survive after verification.
//!
//! # Running
//!
//! ```
//! cargo test --test correctness
//! ```
//!
//! Requires `rg` on PATH. Tests are skipped (not failed) if `rg` is absent.
//!
//! # Test Pattern Set (T011)
//!
//! - Exact literal: `parse_query`
//! - Exact literal: `process_batch`
//! - Multi-word literal: `parse_query(` (literal with punctuation)
//! - Regex alternation: `parse_query|process_batch`
//! - Regex repetition: `parse_quer[yi]` (character class)
//! - Case-insensitive literal: `ParseQuery` (matches parseQuery, PARSE_QUERY, parsequery, ...)
//! - No-match pattern: `xyzzy_no_match_sentinel_42`
//! - Unicode content: `café` (non-ASCII identifier)
//! - Optional prefix: `(foo)?bar` -- CORRECT: index emits Grams("bar") only,
//!   NOT And(Grams("foo"), Grams("bar")). foo is optional so requiring it
//!   would be a false negative. The verifier filters candidates.
//! - Dot-star fallback: `parse.*batch` -- no extractable grams spanning the
//!   `.*`; query router must fall back to full scan. ripline must still find
//!   all rg matches.
//! - Path filter: `parse_query` restricted to `*.py` files only
//! - Gitignore: `parse_query` must NOT find `build/output.txt`

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Oracle helpers
// ---------------------------------------------------------------------------

/// Run `rg` on the corpus and return `(relative_path, line_number)` pairs.
///
/// Returns an empty set if the pattern matches no files. Panics if `rg` is
/// not on PATH (tests skip via `rg_available()` guard before calling this).
fn rg_matches(corpus: &Path, pattern: &str, extra_flags: &[&str]) -> BTreeSet<(String, u32)> {
    let mut cmd = Command::new("rg");
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never");

    // Extra flags (e.g. -F, -i, --glob=*.py) must come before -- and positional args
    for flag in extra_flags {
        cmd.arg(flag);
    }

    // Use the corpus .gitignore so ignored files are excluded
    cmd.arg("--").arg(pattern).arg(corpus);

    let output = cmd.output().expect("rg invocation failed");
    // rg exit code 1 = no matches (not an error)
    if !output.status.success() && output.status.code() != Some(1) {
        panic!(
            "rg failed with status {:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).expect("rg output is not UTF-8");
    parse_rg_output(&stdout, corpus)
}

/// Parse `rg --line-number --no-heading` output into `(relative_path, line_number)` pairs.
fn parse_rg_output(stdout: &str, corpus: &Path) -> BTreeSet<(String, u32)> {
    let mut out = BTreeSet::new();
    for line in stdout.lines() {
        // Format: /abs/path/to/file.rs:42:matched content
        let mut parts = line.splitn(3, ':');
        let path_str = match parts.next() {
            Some(p) => p,
            None => continue,
        };
        let line_num_str = match parts.next() {
            Some(n) => n,
            None => continue,
        };
        let line_num: u32 = match line_num_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let abs = PathBuf::from(path_str);
        let rel = abs
            .strip_prefix(corpus)
            .unwrap_or(&abs)
            .to_string_lossy()
            .into_owned();
        out.insert((rel, line_num));
    }
    out
}

/// Return true if `rg` is available on PATH.
fn rg_available() -> bool {
    Command::new("rg").arg("--version").output().is_ok()
}

/// Absolute path to the fixture corpus.
fn corpus_path() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set; run via cargo test");
    PathBuf::from(manifest).join("tests/fixtures/corpus")
}

// ---------------------------------------------------------------------------
// ripline helpers
// ---------------------------------------------------------------------------

use ripline_rs::index::Index;
use ripline_rs::{Config, SearchOptions};

/// Build a ripline index over the corpus in a temporary directory.
/// Returns the temp dir (kept alive) and the index handle.
fn build_test_index(corpus: &Path) -> (tempfile::TempDir, Index) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let config = Config {
        index_dir: tmp.path().to_path_buf(),
        repo_root: corpus.to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).expect("Index::build failed");
    (tmp, index)
}

/// Run a ripline search and return `(relative_path, line_number)` pairs.
fn ripline_matches(
    index: &Index,
    _corpus: &Path,
    pattern: &str,
    case_insensitive: bool,
    path_glob: Option<&str>,
) -> BTreeSet<(String, u32)> {
    // If the glob is a simple extension filter like "*.py", use file_type
    // (extension match). Otherwise use path_filter (substring match).
    let (path_filter, file_type) = match path_glob {
        Some(g) if g.starts_with("*.") => {
            (None, Some(g.trim_start_matches("*.").to_string()))
        }
        Some(g) => (Some(g.to_string()), None),
        None => (None, None),
    };
    let opts = SearchOptions {
        case_insensitive,
        path_filter,
        file_type,
        ..SearchOptions::default()
    };
    let results = index.search(pattern, &opts).expect("search failed");
    results
        .into_iter()
        .map(|m| {
            let path = m.path.to_string_lossy().into_owned();
            (path, m.line_number)
        })
        .collect()
}

/// Assert that ripline produces a superset of rg results (zero false negatives).
///
/// ripline may return more candidates (false positives that survive to the
/// result set indicate a verifier bug, not an index bug), but it must never
/// miss a file that rg found.
#[allow(dead_code)]
fn assert_no_false_negatives(
    corpus: &Path,
    rg_result: &BTreeSet<(String, u32)>,
    ripline_result: &BTreeSet<(String, u32)>,
    pattern: &str,
) {
    let missed: Vec<_> = rg_result.difference(ripline_result).collect();
    assert!(
        missed.is_empty(),
        "ripline missed {} matches for pattern {:?} on corpus {:?}.\n\
         First missed: {:?}\n\
         rg found {} total, ripline found {}.",
        missed.len(),
        pattern,
        corpus,
        missed.first(),
        rg_result.len(),
        ripline_result.len(),
    );
}

/// Assert exact equality between rg and ripline results.
#[allow(dead_code)]
fn assert_exact_match(
    corpus: &Path,
    rg_result: &BTreeSet<(String, u32)>,
    ripline_result: &BTreeSet<(String, u32)>,
    pattern: &str,
) {
    assert_eq!(
        rg_result, ripline_result,
        "ripline results differ from rg for pattern {:?} on corpus {:?}",
        pattern, corpus
    );
}

// ---------------------------------------------------------------------------
// T011: Test pattern set
//
// These tests define the correctness contract. All tests are currently
// SKIPPED if rg is unavailable. Once Index::build and search are
// implemented (Phases 3-4), remove the early-return stubs and wire in
// the real build_test_index / ripline_matches calls.
// ---------------------------------------------------------------------------

/// Exact literal: `parse_query` appears in 3+ files.
#[test]
fn literal_parse_query() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_result = rg_matches(&corpus, "parse_query", &[]);
    assert!(
        rg_result.len() >= 3,
        "fixture invariant: parse_query must appear in >=3 files, got {}",
        rg_result.len()
    );
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "parse_query", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "parse_query");
}

/// Exact literal: `process_batch` appears in 2+ files.
#[test]
fn literal_process_batch() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_result = rg_matches(&corpus, "process_batch", &[]);
    assert!(
        rg_result.len() >= 2,
        "fixture invariant: process_batch must appear in >=2 files, got {}",
        rg_result.len()
    );
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "process_batch", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "process_batch");
}

/// Literal with punctuation: `parse_query(` -- the `(` is part of the literal.
#[test]
fn literal_with_punctuation() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    // rg treats this as a fixed string with -F
    let rg_result = rg_matches(&corpus, "parse_query(", &["-F"]);
    assert!(
        !rg_result.is_empty(),
        "fixture invariant: parse_query( must appear in at least 1 file"
    );
    // parse_query( contains '(' which is a regex metacharacter, so ripline
    // treats it as regex. Use the escaped form for the regex engine.
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, r"parse_query\(", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "parse_query(");
}

/// Regex alternation: `parse_query|process_batch`.
#[test]
fn regex_alternation() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_result = rg_matches(&corpus, "parse_query|process_batch", &[]);
    assert!(
        !rg_result.is_empty(),
        "fixture invariant: alternation must match at least one file"
    );
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "parse_query|process_batch", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "parse_query|process_batch");
}

/// Regex character class: `parse_quer[yi]` (matches parse_query and parse_queri).
#[test]
fn regex_character_class() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_result = rg_matches(&corpus, "parse_quer[yi]", &[]);
    assert!(!rg_result.is_empty(), "fixture invariant: character class must match at least 1 file");
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "parse_quer[yi]", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "parse_quer[yi]");
}

/// Case-insensitive literal: `-i ParseQuery` matches parseQuery, PARSE_QUERY, etc.
#[test]
fn case_insensitive_literal() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_ci = rg_matches(&corpus, "ParseQuery", &["-i"]);
    let rg_cs = rg_matches(&corpus, "ParseQuery", &[]);
    assert!(
        rg_ci.len() >= rg_cs.len(),
        "case-insensitive must find at least as many matches as case-sensitive"
    );
    assert!(
        rg_ci.len() > 0,
        "fixture invariant: case-insensitive ParseQuery must match at least 1 file"
    );
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "ParseQuery", true, None);
    assert_no_false_negatives(&corpus, &rg_ci, &ripline_result, "ParseQuery (case-insensitive)");
}

/// No-match pattern: must return empty result set.
#[test]
fn no_match_pattern() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_result = rg_matches(&corpus, "xyzzy_no_match_sentinel_42", &[]);
    assert!(
        rg_result.is_empty(),
        "sentinel pattern must not appear in any fixture file"
    );
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result =
        ripline_matches(&index, &corpus, "xyzzy_no_match_sentinel_42", false, None);
    assert!(
        ripline_result.is_empty(),
        "ripline must return empty for no-match sentinel, got {:?}",
        ripline_result
    );
}

/// Unicode content: `café` (non-ASCII in a Python identifier).
#[test]
fn unicode_identifier() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_result = rg_matches(&corpus, "café", &[]);
    assert!(
        !rg_result.is_empty(),
        "fixture invariant: unicode_identifiers.py must contain 'café'"
    );
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "café", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "café");
}

/// Optional prefix pattern: `(foo)?bar`
///
/// IMPORTANT: ripline's HIR walker correctly extracts `Grams("bar")` only,
/// NOT `And(Grams("foo"), Grams("bar"))`.
///
/// Rationale: `foo` is optional. Requiring it in the gram query would
/// produce false negatives for inputs like "bazbar" or "quxbar". The index
/// must return all candidates containing "bar"; the verifier then confirms
/// that each candidate actually matches `(foo)?bar` as a regex.
///
/// Do NOT "fix" this to also require `foo`. That would be a correctness bug.
/// This test explicitly verifies that behavior does not regress.
#[test]
fn optional_prefix_pattern() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    // rg with regex `(foo)?bar` finds all lines containing `bar` or `foobar`
    let rg_result = rg_matches(&corpus, "(foo)?bar", &[]);

    // All rg results contain "bar" (with or without "foo" prefix).
    // Also count bare "bar" matches to show the optional case fires.
    let bar_only = rg_matches(&corpus, r"\bbar\b", &[]);
    let foobar = rg_matches(&corpus, "foobar", &[]);

    // The full result set is the union; foobar is a strict subset.
    let _ = (bar_only, foobar); // checked for clarity

    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "(foo)?bar", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "(foo)?bar");
}

/// `parse.*batch`: the `.*` contributes All which simplifies away, leaving
/// And(Grams("parse"), Grams("batch")) as an indexed regex query.
/// ripline must find every file that rg finds -- no false negatives.
#[test]
fn dot_star_fallback_to_scan() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_result = rg_matches(&corpus, "parse.*batch", &[]);
    assert!(
        !rg_result.is_empty(),
        "fixture invariant: parse.*batch must match at least 1 file \
         (long_line.txt has 'parse_query' and 'process_batch' on the same line)"
    );
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "parse.*batch", false, None);
    assert_no_false_negatives(&corpus, &rg_result, &ripline_result, "parse.*batch");
}

/// Path filter: `parse_query` restricted to `*.py` files.
///
/// Must only return Python files, not Rust, Go, etc.
#[test]
fn path_filter_py_only() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let rg_all = rg_matches(&corpus, "parse_query", &[]);
    let rg_py = rg_matches(&corpus, "parse_query", &["--glob=*.py"]);

    assert!(
        rg_py.len() < rg_all.len(),
        "py filter must return fewer results than unfiltered"
    );
    // All py results must have .py extension
    for (path, _) in &rg_py {
        assert!(
            path.ends_with(".py"),
            "path filter returned non-.py file: {}",
            path
        );
    }
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "parse_query", false, Some("*.py"));
    // ripline path_filter uses extension match; verify subset correctness.
    for (path, _) in &ripline_result {
        assert!(
            path.ends_with(".py"),
            "ripline path filter returned non-.py file: {}",
            path
        );
    }
    assert_no_false_negatives(&corpus, &rg_py, &ripline_result, "parse_query (*.py filter)");
}

/// Gitignore: `build/output.txt` must not appear in results.
///
/// The corpus `.gitignore` ignores `build/`. The indexer must respect it.
#[test]
fn gitignore_excludes_build_dir() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    // rg respects .gitignore by default; build/ should be excluded
    let rg_result = rg_matches(&corpus, "parse_query", &[]);
    for (path, _) in &rg_result {
        assert!(
            !path.starts_with("build/") && !path.contains("/build/"),
            "rg returned gitignored file: {}",
            path
        );
    }
    let (_tmp, index) = build_test_index(&corpus);
    let ripline_result = ripline_matches(&index, &corpus, "parse_query", false, None);
    for (path, _) in &ripline_result {
        assert!(
            !path.starts_with("build/") && !path.contains("/build/"),
            "ripline returned gitignored file: {}",
            path
        );
    }
}

/// Verify the rg oracle itself produces consistent results across two runs.
///
/// This is a meta-test: if rg is non-deterministic on this corpus, the
/// oracle harness is unreliable and we need to investigate.
#[test]
fn oracle_is_deterministic() {
    if !rg_available() {
        eprintln!("SKIP: rg not on PATH");
        return;
    }
    let corpus = corpus_path();
    let run1 = rg_matches(&corpus, "parse_query", &[]);
    let run2 = rg_matches(&corpus, "parse_query", &[]);
    assert_eq!(run1, run2, "rg produced different results on two consecutive runs");
}
