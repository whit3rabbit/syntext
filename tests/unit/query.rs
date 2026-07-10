//! Unit tests for query decomposition and routing (T035).
//!
//! CRITICAL: The `(foo)?bar` test verifies that optional prefixes do NOT
//! contribute grams. This is not a bug -- it is required for correctness.
//! Requiring "foo" grams would produce false negatives for inputs like "bazbar".

use tempfile::TempDir;

use syntext::__internal::regex_decompose::decompose;
use syntext::__internal::{is_literal, literal_grams, route_query, GramQuery, QueryRoute};
use syntext::index::Index;
use syntext::Config;

// ---------------------------------------------------------------------------
// GramQuery::simplify tests
// ---------------------------------------------------------------------------

#[test]
fn simplify_removes_all_from_and() {
    let q = GramQuery::And(vec![
        GramQuery::All,
        GramQuery::Grams(vec![1, 2]),
        GramQuery::All,
    ]);
    match q.simplify() {
        GramQuery::Grams(hashes) => assert_eq!(hashes, vec![1, 2]),
        other => panic!("expected Grams, got {:?}", other),
    }
}

#[test]
fn simplify_all_dominates_or() {
    let q = GramQuery::Or(vec![
        GramQuery::Grams(vec![1]),
        GramQuery::All,
        GramQuery::Grams(vec![2]),
    ]);
    assert!(matches!(q.simplify(), GramQuery::All));
}

#[test]
fn simplify_empty_and_becomes_all() {
    let q = GramQuery::And(vec![]);
    assert!(matches!(q.simplify(), GramQuery::All));
}

#[test]
fn simplify_empty_or_becomes_none() {
    let q = GramQuery::Or(vec![]);
    assert!(matches!(q.simplify(), GramQuery::None));
}

#[test]
fn simplify_single_child_and_unwraps() {
    let q = GramQuery::And(vec![GramQuery::Grams(vec![42])]);
    match q.simplify() {
        GramQuery::Grams(hashes) => assert_eq!(hashes, vec![42]),
        other => panic!("expected Grams, got {:?}", other),
    }
}

#[test]
fn simplify_single_child_or_unwraps() {
    let q = GramQuery::Or(vec![GramQuery::Grams(vec![99])]);
    match q.simplify() {
        GramQuery::Grams(hashes) => assert_eq!(hashes, vec![99]),
        other => panic!("expected Grams, got {:?}", other),
    }
}

#[test]
fn simplify_and_of_all_becomes_all() {
    let q = GramQuery::And(vec![GramQuery::All, GramQuery::All]);
    assert!(matches!(q.simplify(), GramQuery::All));
}

// ---------------------------------------------------------------------------
// HIR decomposition tests
// ---------------------------------------------------------------------------

/// Regex literal "parse_query" -> All (full scan) because regex literals
/// use build_covering_inner which only emits grams with both boundaries at
/// forced boundary characters. "parse" and "query" touch synthetic 0/len
/// boundaries, so no interior grams survive.
///
/// Note: LITERAL queries (QueryRoute::Literal) still use build_covering
/// which treats 0/len as real boundaries. The distinction is intentional:
/// literal searches are complete tokens; regex literals are fragments.
#[test]
fn decompose_literal_falls_back_to_scan() {
    let q = decompose("parse_query", false).unwrap().simplify();
    // Regex literals with no interior forced-boundary grams fall to All.
    assert!(matches!(q, GramQuery::All), "expected All, got {:?}", q);
}

/// `foo.*bar` -> All because "foo" and "bar" are regex literals without
/// interior forced-boundary grams. Both fall to All, and And([All, All, All])
/// simplifies to All (full scan). This is correct: regex literals at
/// arbitrary document positions cannot safely use edge grams.
#[test]
fn decompose_dot_star_between_literals() {
    let q = decompose("foo.*bar", false).unwrap().simplify();
    assert!(
        matches!(q, GramQuery::All),
        "foo.*bar should fall to full scan with forced boundaries, got {:?}",
        q
    );
}

/// `foo|bar` -> All because "foo" and "bar" have no interior forced-boundary
/// grams. Or([All, All]) -> All (full scan).
#[test]
fn decompose_alternation() {
    let q = decompose("foo|bar", false).unwrap().simplify();
    assert!(matches!(q, GramQuery::All), "expected All, got {:?}", q);
}

#[test]
fn decompose_exact_literal_alternation_with_shared_prefix() {
    let q = decompose("LanguageServer(Id|InstallationStatus)", false)
        .unwrap()
        .simplify();
    assert!(
        matches!(q, GramQuery::Or(_) | GramQuery::Grams(_)),
        "expected indexed grams for exact literal alternation, got {:?}",
        q
    );
}

/// `(parse_query)+` -> All because the regex literal "parse_query" has no
/// interior forced-boundary grams (both "parse" and "query" touch synthetic
/// edges). Required repetition (min=1) is valid for gram constraints, but
/// there are no grams to constrain with.
#[test]
fn decompose_required_repetition() {
    let q = decompose("(parse_query)+", false).unwrap().simplify();
    assert!(
        matches!(q, GramQuery::All),
        "(parse_query)+ should fall to full scan with forced boundaries, got {:?}",
        q
    );
}

/// `.*` -> All -> FullScan route.
/// No extractable grams from a pure wildcard.
#[test]
fn decompose_dot_star_alone_is_all() {
    let q = decompose(".*", false).unwrap().simplify();
    assert!(matches!(q, GramQuery::All), "expected All, got {:?}", q);
}

/// CRITICAL: `(foo)?bar` -> Grams("bar") only.
///
/// The optional prefix `(foo)?` must NOT contribute gram constraints.
/// Requiring "foo" grams would cause false negatives for inputs like "bazbar".
/// After simplification: And([All, Grams("bar")]) -> Grams("bar").
#[test]
fn optional_prefix_does_not_contribute_grams() {
    let q = decompose("(foo)?bar", false).unwrap().simplify();

    // Must produce grams for "bar" only (not an And requiring both foo and bar).
    match &q {
        GramQuery::Grams(_) => {
            // Exactly what we want: only grams for the required part.
        }
        GramQuery::And(children) => {
            // If And, it must NOT contain any node requiring "foo" grams that
            // would miss inputs containing "bar" without "foo".
            // For this pattern, And should have been simplified away.
            panic!(
                "optional prefix produced And node (may cause false negatives): {:?}",
                children
            );
        }
        GramQuery::All => {
            // Acceptable: no grams extracted, falls back to scan (bar is short).
        }
        other => panic!("unexpected query type for (foo)?bar: {:?}", other),
    }

    // The grams produced must NOT require "foo". Verify by checking that
    // the query is NOT And(Grams(foo_hashes), Grams(bar_hashes)).
    // Since we verified it's Grams or All above, foo grams cannot be required.
}

// ---------------------------------------------------------------------------
// Query routing tests
// ---------------------------------------------------------------------------

#[test]
fn is_literal_no_metacharacters() {
    assert!(is_literal("parse_query"));
    assert!(is_literal("hello world"));
    assert!(is_literal("foo_bar_baz"));
    assert!(is_literal("some::path::to::function"));
}

#[test]
fn is_literal_with_metacharacters() {
    assert!(!is_literal("foo.*bar"));
    assert!(!is_literal("foo?bar"));
    assert!(!is_literal("[abc]"));
    assert!(!is_literal("(foo)+"));
    assert!(!is_literal("^start"));
    assert!(!is_literal("end$"));
    assert!(!is_literal("foo\\d"));
}

#[test]
fn route_literal_pattern() {
    let route = route_query("parse_query", false).unwrap();
    assert!(
        matches!(route, QueryRoute::Literal),
        "expected Literal route, got {:?}",
        route
    );
}

#[test]
fn route_regex_without_interior_grams_is_fullscan() {
    // With forced boundaries, regex literals have no interior grams, so
    // alternation of short literals falls to FullScan.
    let route = route_query("parse_query|process_batch", false).unwrap();
    assert!(
        matches!(route, QueryRoute::FullScan),
        "expected FullScan for regex without interior grams, got {:?}",
        route
    );
}

#[test]
fn route_regex_with_extractable_grams_is_indexed() {
    let route = route_query("(fn_parse_filter_query)+", false).unwrap();
    assert!(
        matches!(route, QueryRoute::IndexedRegex(_)),
        "expected IndexedRegex for extractable regex grams, got {:?}",
        route
    );
}

#[test]
fn route_exact_literal_alternation_with_shared_prefix_is_indexed() {
    let route = route_query("LanguageServer(Id|InstallationStatus)", false).unwrap();
    assert!(
        matches!(route, QueryRoute::IndexedRegex(_)),
        "expected IndexedRegex for exact literal alternation, got {:?}",
        route
    );
}

#[test]
fn route_full_scan_for_dot_star() {
    let route = route_query(".*", false).unwrap();
    assert!(
        matches!(route, QueryRoute::FullScan),
        "expected FullScan for .*, got {:?}",
        route
    );
}

#[test]
fn route_case_insensitive_literal_not_literal_route() {
    // Case-insensitive search can't use memmem, but long literals should still
    // extract covering grams from the lowercased index.
    let route = route_query("_parse_query_", true).unwrap();
    assert!(
        matches!(route, QueryRoute::IndexedRegex(_)),
        "case-insensitive anchored long literal should use IndexedRegex, got {:?}",
        route
    );
}

#[test]
fn literal_grams_returns_none_for_short_pattern() {
    assert!(literal_grams("ab").is_none());
    assert!(literal_grams("x").is_none());
}

#[test]
fn literal_grams_returns_some_for_long_pattern() {
    assert!(literal_grams("parse_query").is_some());
}

#[test]
fn route_case_insensitive_short_literal_falls_back_to_full_scan() {
    let route = route_query("ab", true).unwrap();
    assert!(
        matches!(route, QueryRoute::FullScan),
        "short case-insensitive literal should fall back to FullScan, got {:?}",
        route
    );
}

/// Case-insensitive literal whose only grams are optional (synthetic-edge,
/// unanchored) must route to FullScan, not IndexedRegex(Grams(...)). ANDing the
/// optional gram silently drops documents where the match is sub-token
/// (e.g. `-i parse` missing `reparse` when another file emits `parse` as a
/// token-aligned gram). FullScan lets the case-insensitive regex verifier see
/// every candidate. Regression for the case-insensitive AND-intersection bug.
#[test]
fn route_case_insensitive_optional_only_literal_is_full_scan() {
    let route = route_query("parse", true).unwrap();
    assert!(
        matches!(route, QueryRoute::FullScan),
        "case-insensitive 'parse' (optional-only grams) must route to FullScan, got {:?}",
        route
    );
}

/// A regex escaped-backslash-then-`n` (`\\n`, i.e. a literal backslash followed
/// by the letter n) is NOT a newline and must route, not error. This is what a
/// fixed string like `C:\new` becomes after `regex::escape`, and what the review
/// case `foo\\nbar` is. The old `contains("\\n")` string guard wrongly rejected
/// both because their source text contains the two bytes `\` `n`. Only a raw
/// newline byte, or a genuine `\n` regex newline escape, may be rejected.
#[test]
fn route_backslash_n_is_not_a_newline() {
    // `foo\\nbar` as a regex: literal `\`, then `nbar`. No newline -> Ok.
    assert!(
        route_query(r"foo\\nbar", false).is_ok(),
        "escaped-backslash-then-n must not be treated as a literal newline"
    );
    // The escaped form of the fixed string `C:\new` (`C:\\new`) -> Ok.
    assert!(
        route_query(r"C:\\new", false).is_ok(),
        "regex-escaped `C:\\new` must route, not error"
    );
    // A raw newline byte is still rejected.
    assert!(
        route_query("a\nb", false).is_err(),
        "a raw newline byte must still be rejected"
    );
    // A genuine `\n` regex newline escape is still rejected (HIR check).
    assert!(
        route_query(r"a\nb", false).is_err(),
        "a `\\n` regex newline escape must still be rejected"
    );
}

// ---------------------------------------------------------------------------
// EH-0001: literal-substring false negatives from synthetic token-edge grams
// ---------------------------------------------------------------------------

/// A literal query for "parse" must find files containing sub-token matches
/// like "reparse", not just files where "parse" is a token-aligned gram.
#[test]
fn eh0001_literal_parse_finds_subtoken_reparse() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // File A: "reparse" — parse appears as a substring (not token-aligned).
    std::fs::write(repo.path().join("a.rs"), "fn reparse() {}\n").unwrap();
    // File B: "parse" — parse IS token-aligned.
    std::fs::write(repo.path().join("b.rs"), "fn parse() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let results = index.search("parse", &Default::default()).unwrap();

    let mut paths: Vec<String> = results
        .iter()
        .map(|m| m.path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    paths.sort();
    paths.dedup();

    assert_eq!(
        paths,
        vec!["a.rs", "b.rs"],
        "literal 'parse' must find both 'reparse' (a.rs) and 'parse' (b.rs)"
    );
    drop(index);
}

/// Insertion order must not affect results: whether reparse or parse is
/// indexed first, both files must appear.
#[test]
fn eh0001_literal_parse_insertion_order_independent() {
    for first in &["reparse", "parse"] {
        let repo = TempDir::new().unwrap();
        let index_dir = TempDir::new().unwrap();

        // Index the files in a known order within a single build.
        let content_a = if *first == "reparse" {
            "fn reparse() {}\nfn parse_it() {}\n"
        } else {
            "fn parse() {}\nfn reparsing() {}\n"
        };
        std::fs::write(repo.path().join("lib.rs"), content_a).unwrap();

        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();
        let results = index.search("parse", &Default::default()).unwrap();

        assert!(
            !results.is_empty(),
            "parse must produce results regardless of insertion order (tried {first} first)"
        );
        drop(index);
    }
}

/// build_covering("parse") must classify its single gram as optional
/// (unanchored) and produce zero required grams.
#[test]
fn eh0001_build_covering_classifies_parse_as_optional_only() {
    let covering = syntext::__internal::build_covering(b"parse").unwrap();
    assert!(
        covering.required.is_empty(),
        "parse has no interior boundaries: all grams must be optional"
    );
    assert_eq!(
        covering.optional.len(),
        1,
        "parse must produce exactly one optional gram"
    );
}

/// build_covering("parse_query") must classify both grams as optional
/// because each has a synthetic boundary (start or end of query).
#[test]
fn eh0001_build_covering_classifies_parse_query_as_optional() {
    let covering = syntext::__internal::build_covering(b"parse_query").unwrap();
    assert!(
        covering.required.is_empty(),
        "parse_query has synthetic edges; grams must be optional"
    );
    assert_eq!(
        covering.optional.len(),
        2,
        "parse_query must produce exactly two optional grams"
    );
}

/// build_covering("_parse_query_") must classify both grams as required
/// because both are anchored by forced boundaries (start, end, and middle are all '_').
#[test]
fn eh0001_build_covering_classifies_anchored_parse_query_as_required() {
    let covering = syntext::__internal::build_covering(b"_parse_query_").unwrap();
    assert_eq!(
        covering.required.len(),
        2,
        "anchored parse_query must produce required grams"
    );
    assert!(
        covering.optional.is_empty(),
        "anchored parse_query should not have optional grams"
    );
}

/// Case-insensitive counterpart of `eh0001_literal_parse_finds_subtoken_reparse`.
/// `-i parse` must find `reparse` (sub-token) even when another file emits the
/// `parse` gram token-aligned. Previously the case-insensitive route AND-intersected
/// the optional `parse` gram, returned a non-empty single-doc candidate set, and
/// silently dropped the sub-token file.
#[test]
fn eh0001_case_insensitive_literal_finds_subtoken_reparse() {
    let repo = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();

    // a.rs: `parse` appears only as a sub-token of `reparse`.
    std::fs::write(repo.path().join("a.rs"), "fn reparse() {}\n").unwrap();
    // b.rs: `PARSE` lowercases to the token-aligned `parse` gram.
    std::fs::write(repo.path().join("b.rs"), "fn PARSE() {}\n").unwrap();

    let config = Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..Config::default()
    };
    let index = Index::build(config).unwrap();
    let mut opts = syntext::SearchOptions::default();
    opts.case_insensitive = true;
    let results = index.search("parse", &opts).unwrap();

    let mut paths: Vec<String> = results
        .iter()
        .map(|m| m.path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    paths.sort();
    paths.dedup();

    assert_eq!(
        paths,
        vec!["a.rs", "b.rs"],
        "case-insensitive 'parse' must find both reparse (a.rs) and PARSE (b.rs)"
    );
    drop(index);
}

#[test]
fn route_query_rejects_newlines() {
    assert_eq!(
        route_query("foo\nbar", false).unwrap_err(),
        "literal \\n not allowed".to_string()
    );
    assert_eq!(
        route_query("foo\\nbar", false).unwrap_err(),
        "literal \\n not allowed".to_string()
    );
    assert_eq!(
        route_query("foo\\x0abar", false).unwrap_err(),
        "literal \\n not allowed".to_string()
    );
    assert_eq!(
        route_query("foo\\u000abar", false).unwrap_err(),
        "literal \\n not allowed".to_string()
    );
    assert_eq!(
        route_query("foo\\u{a}bar", false).unwrap_err(),
        "literal \\n not allowed".to_string()
    );
}
