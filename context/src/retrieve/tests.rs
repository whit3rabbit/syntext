use std::sync::Arc;

use super::*;
use crate::compile::{CompileOptions, SourceDoc};
use crate::store::{ContextStore, StoreOptions};
use crate::types::{Role, Scope};

const ARCHITECTURE: &str = "# Architecture

## Query routing

The router inspects each query and picks one of three execution paths:
literal, indexed regex, or full scan. Literal queries use covering grams.

## Overlay

The overlay is rebuilt from all dirty files on every commit batch. Readers
see the new overlay atomically through a snapshot swap, so a search never
observes a half-built overlay.

## Credentials

The auth service validates API credentials before any request reaches the
router. Expired credentials are rejected with a 401.
";

const ROUTER: &str = "use crate::grams::covering;

/// Pick an execution path for a query.
pub fn route_query(pattern: &str) -> Route {
    if is_literal(pattern) {
        Route::Literal(covering(pattern))
    } else {
        Route::FullScan
    }
}

/// True when the pattern has no regex metacharacters.
fn is_literal(pattern: &str) -> bool {
    !pattern.contains(['*', '+', '?', '(', '['])
}
";

const FACTS: &str = "# People

Alice works for Meridian Labs. She leads the platform team.

# Deals

Meridian Labs acquired Northstar Systems in 2021 after a long negotiation.

# History

Northstar Systems was founded by Robert in 1999.

# Unrelated

The cafeteria serves lunch at noon.
";

fn store_with_corpus() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(
        dir.path(),
        StoreOptions {
            sync: false,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    store
        .upsert(vec![
            SourceDoc::file("repo", "docs/architecture.md", ARCHITECTURE).unwrap(),
            SourceDoc::file("repo", "src/router.rs", ROUTER).unwrap(),
            SourceDoc::note("facts", FACTS).unwrap(),
        ])
        .unwrap();
    (dir, store)
}

fn ask(store: &ContextStore, question: &str, opts: &RetrieveOptions) -> Retrieved {
    retrieve(
        &store.snapshot(),
        None,
        &CompileOptions::default(),
        question,
        opts,
    )
    .unwrap()
}

#[test]
fn where_questions_find_the_definition() {
    let (_dir, store) = store_with_corpus();
    let r = ask(
        &store,
        "Where is route_query defined?",
        &RetrieveOptions::default(),
    );
    let top = &r.passages[0];
    assert_eq!(top.doc_key, "repo:src/router.rs");
    assert_eq!(top.symbol.as_deref(), Some("route_query"));
    assert_eq!(top.kind, PassageKind::Definition);
    assert_eq!(r.query.intent, QueryIntent::Locate);
    assert!(top.signals.symbol > 0.9);
    assert!(
        top.focus_lines.contains(&4),
        "signature line is a focus line: {:?}",
        top.focus_lines
    );
    drop(store);
}

#[test]
fn how_questions_find_the_explaining_section() {
    let (_dir, store) = store_with_corpus();
    let r = ask(
        &store,
        "How does the overlay get rebuilt?",
        &RetrieveOptions::default(),
    );
    let top = &r.passages[0];
    assert_eq!(top.doc_key, "repo:docs/architecture.md");
    assert!(top.breadcrumb.ends_with("Overlay"), "{}", top.breadcrumb);
    assert!(
        top.text.contains("snapshot swap"),
        "whole paragraph, not a sentence"
    );
    drop(store);
}

#[test]
fn stemming_bridges_word_forms() {
    let (_dir, store) = store_with_corpus();
    let r = ask(
        &store,
        "Which service handles validation of credentials?",
        &RetrieveOptions::default(),
    );
    assert!(
        r.passages[0].breadcrumb.ends_with("Credentials"),
        "{:?}",
        r.passages[0].breadcrumb
    );
    drop(store);
}

#[test]
fn multi_hop_question_gathers_the_whole_chain() {
    let (_dir, store) = store_with_corpus();
    let r = ask(
        &store,
        "Who founded the company acquired by Alice's employer?",
        &RetrieveOptions::default(),
    );
    let texts: Vec<&str> = r.passages.iter().take(5).map(|p| p.text.as_str()).collect();
    for needle in [
        "works for Meridian Labs",
        "acquired Northstar Systems",
        "founded by Robert",
    ] {
        assert!(
            texts.iter().any(|t| t.contains(needle)),
            "missing {needle:?} in {texts:#?}"
        );
    }
    assert!(
        r.query.entities.iter().any(|e| e == "Alice"),
        "{:?}",
        r.query.entities
    );
    let founded = r
        .passages
        .iter()
        .find(|p| p.text.contains("founded by Robert"))
        .unwrap();
    let chain = founded.chain.as_ref().expect("reached through the graph");
    assert!(!chain.steps.is_empty());
    assert!(!r
        .passages
        .iter()
        .take(3)
        .any(|p| p.text.contains("cafeteria")));
    drop(store);
}

#[test]
fn graph_off_still_retrieves_lexically() {
    let (_dir, store) = store_with_corpus();
    let opts = RetrieveOptions {
        hops: 0,
        ..RetrieveOptions::default()
    };
    let r = ask(&store, "Where is route_query defined?", &opts);
    assert_eq!(r.passages[0].symbol.as_deref(), Some("route_query"));
    assert!(r.passages.iter().all(|p| p.chain.is_none()));
    drop(store);
}

#[test]
fn scopes_and_exclusions_filter_results() {
    let (_dir, store) = store_with_corpus();
    let scope = Scope::app("assistant").unwrap();
    store
        .append_turn(
            "s1",
            Role::User,
            scope.clone(),
            "My deploy key rotates every Tuesday.",
            None,
        )
        .unwrap();
    let q = "When does the deploy key rotate?";
    let r = ask(&store, q, &RetrieveOptions::default());
    assert!(
        r.passages.iter().all(|p| p.doc_key != "turn:1"),
        "ungranted scope leaked"
    );
    let granted = RetrieveOptions {
        scopes: crate::types::ScopeFilter::default().grant(scope),
        ..RetrieveOptions::default()
    };
    let r = ask(&store, q, &granted);
    assert_eq!(r.passages[0].doc_key, "turn:1");
    assert_eq!(r.passages[0].trust, crate::types::Trust::Memory);
    let excluded = RetrieveOptions {
        exclude: vec![crate::types::DocKey::new("turn:1").unwrap()],
        ..granted
    };
    let r = ask(&store, q, &excluded);
    assert!(r.passages.iter().all(|p| p.doc_key != "turn:1"));
    drop(store);
}

#[test]
fn results_are_deterministic() {
    let (_dir, store) = store_with_corpus();
    let opts = RetrieveOptions::default();
    let a: Vec<(String, u32)> = ask(&store, "overlay snapshot readers", &opts)
        .passages
        .iter()
        .map(|p| (p.doc_key.clone(), p.lines.0))
        .collect();
    for _ in 0..5 {
        let b: Vec<(String, u32)> = ask(&store, "overlay snapshot readers", &opts)
            .passages
            .iter()
            .map(|p| (p.doc_key.clone(), p.lines.0))
            .collect();
        assert_eq!(a, b);
    }
    drop(store);
}

#[test]
fn empty_and_unknown_questions_return_nothing() {
    let (_dir, store) = store_with_corpus();
    assert!(ask(&store, "", &RetrieveOptions::default())
        .passages
        .is_empty());
    assert!(ask(&store, "zyxwvut qqqq", &RetrieveOptions::default())
        .passages
        .is_empty());
    drop(store);
}

#[test]
fn syntext_bridge_prefers_live_content_over_a_stale_store() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/router.rs"), ROUTER).unwrap();
    std::fs::write(root.join("src/other.rs"), "fn unrelated() {}\n").unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(
        store_dir.path(),
        StoreOptions {
            sync: false,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    let source = crate::compile::DirectorySource::new("repo", root).unwrap();
    store.sync_directory(&source).unwrap();

    let index_dir = tempfile::tempdir().unwrap();
    let mut cfg = syntext::Config::new(index_dir.path().to_path_buf(), root.to_path_buf());
    cfg.strict_permissions = false;
    let index = Arc::new(syntext::index::Index::build(cfg).unwrap());
    let bridge = SyntextSource::new(Arc::clone(&index), "repo").unwrap();

    // Fresh store: the exact hit lands on the stored passage.
    let r = retrieve(
        &store.snapshot(),
        Some(&bridge),
        &CompileOptions::default(),
        "where is route_query",
        &RetrieveOptions::default(),
    )
    .unwrap();
    let top = &r.passages[0];
    assert!(!top.live);
    assert_eq!(top.signals.exact, 1.0);

    // The file changes on disk and the syntext index learns about it, but
    // the store has not been re-synced: retrieval serves the live version.
    let edited = ROUTER.replace("Route::FullScan", "Route::IndexedRegex");
    std::fs::write(root.join("src/router.rs"), &edited).unwrap();
    index
        .notify_change_immediate(&root.join("src/router.rs"))
        .unwrap();
    let r = retrieve(
        &store.snapshot(),
        Some(&bridge),
        &CompileOptions::default(),
        "where is route_query",
        &RetrieveOptions::default(),
    )
    .unwrap();
    let top = &r.passages[0];
    assert!(top.live, "stale stored copy replaced by live content");
    assert!(top.text.contains("IndexedRegex"));
    assert!(r
        .passages
        .iter()
        .all(|p| !p.text.contains("Route::FullScan")));
    drop(index);
    drop(store);
}
