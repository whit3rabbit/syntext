use super::*;
use crate::retrieve::query::analyze_query;

fn positions(pairs: &[(u32, usize)]) -> Vec<(u32, usize)> {
    pairs.to_vec()
}

#[test]
fn proximity_prefers_tight_windows() {
    let tight = proximity(&mut positions(&[(3, 0), (4, 1), (40, 0)]), 2);
    let loose = proximity(&mut positions(&[(3, 0), (30, 1)]), 2);
    assert!((tight - 1.0).abs() < 1e-6, "{tight}");
    assert!(loose < tight * 0.5, "{loose}");
    assert_eq!(
        proximity(&mut positions(&[(1, 0), (2, 0)]), 2),
        0.0,
        "one distinct term"
    );
    assert_eq!(proximity(&mut positions(&[(1, 0)]), 1), 0.0);
    let partial = proximity(&mut positions(&[(1, 0), (2, 1)]), 4);
    assert!(
        (partial - 0.5).abs() < 1e-6,
        "two of four terms adjacent: {partial}"
    );
}

#[test]
fn bigrams_allow_one_gap_and_respect_order() {
    assert_eq!(bigram_fraction(&[(1, 0), (2, 1)], 2), 1.0);
    assert_eq!(
        bigram_fraction(&[(1, 0), (3, 1)], 2),
        1.0,
        "one stopword gap"
    );
    assert_eq!(bigram_fraction(&[(1, 0), (5, 1)], 2), 0.0);
    assert_eq!(bigram_fraction(&[(2, 0), (1, 1)], 2), 0.0, "order matters");
}

#[test]
fn symbol_matching() {
    let q = analyze_query("where is parse_query defined");
    assert_eq!(symbol_match(Some("parseQuery"), &q), 1.0);
    assert_eq!(symbol_match(Some("other"), &q), 0.0);
    let q = analyze_query("how do we parse the query string");
    assert_eq!(symbol_match(Some("parse_query"), &q), 0.6);
    assert_eq!(symbol_match(None, &q), 0.0);
}

#[test]
fn unrelated_passages_score_zero() {
    let s = Signals {
        structure: 1.0,
        recency: 1.0,
        ..Signals::default()
    };
    assert_eq!(
        combine(&s, &RankWeights::default(), QueryIntent::General),
        0.0
    );
    let s = Signals { coverage: 1.0, ..s };
    assert!(combine(&s, &RankWeights::default(), QueryIntent::General) > 0.0);
}

#[test]
fn vendored_paths_are_discounted() {
    assert_eq!(path_prior(Some("src/lib.rs"), false), 1.0);
    assert_eq!(path_prior(None, false), 1.0);
    assert!(path_prior(Some("vendor/x/lib.rs"), false) < 1.0);
    assert!(is_vendored("app.min.js"));
    assert!(!is_vendored("src/builder.rs"));
}

#[test]
fn test_code_is_discounted_unless_the_question_is_about_tests() {
    for path in [
        "tests/integration/cli.rs",
        "src/search/tests.rs",
        "src/index/compact_tests.rs",
        "pkg/router/router_test.go",
        "app/test_models.py",
        "web/src/button.test.tsx",
        "web/src/__tests__/button.tsx",
        "core/src/test/java/a/RouterTest.java",
    ] {
        assert!(is_test_path(path), "{path}");
        assert!(path_prior(Some(path), false) < 1.0, "{path}");
        assert_eq!(path_prior(Some(path), true), 1.0, "{path}");
    }
    for path in [
        "src/latest.rs",
        "src/attest.py",
        "src/contest/mod.rs",
        "Test.java",
    ] {
        assert!(!is_test_path(path), "{path}");
    }
    for question in [
        "which tests cover the router?",
        "where is the testing harness?",
        "update the fixtures for specs",
        "why does test_route_query fail?",
    ] {
        assert!(asks_about_tests(&analyze_query(question)), "{question}");
    }
    assert!(!asks_about_tests(&analyze_query(
        "where is route_query defined?"
    )));
}

#[test]
fn intent_priors_rank_kinds() {
    assert!(
        structure_prior(QueryIntent::Locate, PassageKind::Definition)
            > structure_prior(QueryIntent::Locate, PassageKind::Prose)
    );
    assert!(
        structure_prior(QueryIntent::Explain, PassageKind::Prose)
            > structure_prior(QueryIntent::Explain, PassageKind::Code)
    );
    assert!(
        structure_prior(QueryIntent::Recall, PassageKind::Turn)
            > structure_prior(QueryIntent::Recall, PassageKind::Prose)
    );
}
