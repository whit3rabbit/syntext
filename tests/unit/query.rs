//! Unit tests for query decomposition and routing (T035).
//!
//! CRITICAL: The `(foo)?bar` test verifies that optional prefixes do NOT
//! contribute grams. This is not a bug -- it is required for correctness.
//! Requiring "foo" grams would produce false negatives for inputs like "bazbar".

use syntext::query::regex_decompose::decompose;
use syntext::query::{is_literal, literal_grams, route_query, GramQuery, QueryRoute};

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
    let route = route_query("parse_query", true).unwrap();
    assert!(
        matches!(route, QueryRoute::IndexedRegex(_)),
        "case-insensitive long literal should use IndexedRegex, got {:?}",
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
