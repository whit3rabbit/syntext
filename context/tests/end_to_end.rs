//! End-to-end behaviour through the public API: sync a directory, assemble
//! prompts for small windows, follow evidence chains, stay fresh across
//! edits, and survive process restarts.

use std::path::Path;
use std::sync::Arc;

use syntext_context::{
    ChatRequest, ContextEngine, ContextStore, DirectorySource, PrepareRequest, Role, SourceDoc,
    StoreOptions,
};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn fixture(root: &Path) {
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(
        root,
        "README.md",
        "# Billing service\n\nThe billing service charges customers monthly.\n\n## Retries\n\nFailed charges are retried three times with exponential backoff, unless the card was reported stolen, in which case the charge is never retried.\n\n## Contacts\n\nPriya Raman owns the billing service. Priya Raman reports to Tomas Ek.\n\nTomas Ek manages the Payments Platform group.\n",
    );
    write(
        root,
        "src/retry.rs",
        "/// Maximum attempts for a failed charge.\npub const MAX_ATTEMPTS: u32 = 3;\n\n/// Decide whether a failed charge should be retried.\npub fn should_retry(attempt: u32, stolen: bool) -> bool {\n    !stolen && attempt < MAX_ATTEMPTS\n}\n\n/// Backoff before the next attempt, in seconds.\npub fn backoff_secs(attempt: u32) -> u64 {\n    2u64.pow(attempt)\n}\n",
    );
    write(
        root,
        "src/invoice.rs",
        "use crate::retry::should_retry;\n\npub struct Invoice {\n    pub amount_cents: u64,\n}\n\nimpl Invoice {\n    pub fn charge(&self, attempt: u32) -> bool {\n        should_retry(attempt, false)\n    }\n}\n",
    );
}

fn opts() -> StoreOptions {
    StoreOptions {
        sync: false,
        ..StoreOptions::default()
    }
}

#[test]
fn small_window_prompt_keeps_the_qualification() {
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    store
        .sync_directory(&DirectorySource::new("repo", repo.path()).unwrap())
        .unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();

    let turn = engine
        .assemble(&ChatRequest::new("Are failed charges retried?", 4096).system("Be accurate."))
        .unwrap();
    let user = turn.messages.last().unwrap();
    assert_eq!(user.role, Role::User);
    assert!(user
        .content
        .ends_with("Question: Are failed charges retried?"));
    // The whole paragraph arrives, exception included: no sentence trimming.
    assert!(user.content.contains("retried three times"));
    assert!(user.content.contains("never retried"));
    assert!(turn.prompt_tokens <= 4096 - turn.budget.generation);
    drop(engine);
    drop(store);
}

#[test]
fn locate_questions_cite_the_definition_and_its_callers() {
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    store
        .sync_directory(&DirectorySource::new("repo", repo.path()).unwrap())
        .unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let packet = engine
        .prepare(&PrepareRequest::new("Where is should_retry defined?", 1500))
        .unwrap();
    let first = &packet.receipt.evidence[0];
    assert_eq!(first.doc_key, "repo:src/retry.rs");
    assert!(packet.evidence_text.contains("pub fn should_retry"));
    assert!(
        packet
            .blocks
            .iter()
            .any(|b| b.doc_key == "repo:src/invoice.rs"),
        "a caller is included as supporting evidence"
    );
    drop(engine);
    drop(store);
}

#[test]
fn multi_hop_chains_reach_the_answer() {
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    store
        .sync_directory(&DirectorySource::new("repo", repo.path()).unwrap())
        .unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let packet = engine
        .prepare(&PrepareRequest::new(
            "Which group does Priya Raman's manager manage?",
            1500,
        ))
        .unwrap();
    assert!(
        packet.evidence_text.contains("Payments Platform"),
        "{}",
        packet.evidence_text
    );
    assert!(packet.receipt.entities.iter().any(|e| e == "Priya Raman"));
    drop(engine);
    drop(store);
}

#[test]
fn resync_serves_new_content_and_forgets_deleted_files() {
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    let source = DirectorySource::new("repo", repo.path()).unwrap();
    store.sync_directory(&source).unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();

    write(
        repo.path(),
        "src/retry.rs",
        "/// Maximum attempts for a failed charge.\npub const MAX_ATTEMPTS: u32 = 5;\n",
    );
    std::fs::remove_file(repo.path().join("src/invoice.rs")).unwrap();
    let report = store.sync_directory(&source).unwrap();
    assert_eq!((report.modified, report.removed), (1, 1));
    let packet = engine
        .prepare(&PrepareRequest::new("MAX_ATTEMPTS value", 1000))
        .unwrap();
    assert!(packet.evidence_text.contains("MAX_ATTEMPTS: u32 = 5"));
    assert!(!packet.evidence_text.contains("= 3;"));
    assert!(packet
        .blocks
        .iter()
        .all(|b| b.doc_key != "repo:src/invoice.rs"));
    drop(engine);
    drop(store);
}

#[test]
fn a_second_process_sees_the_same_evidence() {
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let dir = tempfile::tempdir().unwrap();
    let question = "How many times are failed charges retried?";
    let first = {
        let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
        store
            .sync_directory(&DirectorySource::new("repo", repo.path()).unwrap())
            .unwrap();
        let engine = ContextEngine::builder(Arc::clone(&store)).build();
        let p = engine
            .prepare(&PrepareRequest::new(question, 1200))
            .unwrap();
        drop(engine);
        drop(store);
        p
    };
    // Fresh handle, read-only: everything comes off disk.
    let store = Arc::new(ContextStore::open_read_only(dir.path(), opts()).unwrap());
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let second = engine
        .prepare(&PrepareRequest::new(question, 1200))
        .unwrap();
    assert_eq!(first.evidence_text, second.evidence_text);
    assert_eq!(first.receipt.generation, second.receipt.generation);
    store.verify().unwrap();
    drop(engine);
    drop(store);
}

#[test]
fn every_hop_of_an_evidence_chain_reaches_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    let mut docs = vec![
        SourceDoc::note(
            "staff",
            "# Staff\n\nAlice works for Meridian Labs as a data engineer.\n",
        )
        .unwrap(),
        SourceDoc::note(
            "deals",
            "# Deals\n\nMeridian Labs acquired Northstar Systems in 2019.\n",
        )
        .unwrap(),
        SourceDoc::note(
            "history",
            "# History\n\nNorthstar Systems was founded by Robert Chen in 2004.\n",
        )
        .unwrap(),
    ];
    // Passages sharing the question's common words but no entity: they
    // outscore the later hops lexically.
    for i in 0..30 {
        docs.push(
            SourceDoc::note(
                &format!("noise{i}"),
                format!("# Note {i}\n\nThe company founded a new team and the employer acquired tools for project {i}.\n"),
            )
            .unwrap(),
        );
    }
    store.upsert(docs).unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let packet = engine
        .prepare(&PrepareRequest::new(
            "Who founded the company acquired by Alice's employer?",
            600,
        ))
        .unwrap();
    let keys: Vec<&str> = packet.blocks.iter().map(|b| b.doc_key.as_str()).collect();
    for hop in ["note:staff", "note:deals", "note:history"] {
        assert!(keys.contains(&hop), "{hop} missing from {keys:?}");
    }
    assert!(packet.evidence_text.contains("Robert Chen"));
    drop(engine);
    drop(store);
}

#[test]
fn chain_hops_in_other_words_still_reach_the_prompt() {
    // The question says "founded" and "acquired"; the evidence says
    // "started" and "bought". Each hop shares no word with the question.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    let mut docs = vec![
        SourceDoc::note(
            "staff",
            "# Staff\n\nBruno Moreau joined Halcyon Labs in 2015.\n",
        )
        .unwrap(),
        SourceDoc::note(
            "deals",
            "# Deals\n\nIn 2019, Halcyon Labs bought Kestrel Robotics.\n",
        )
        .unwrap(),
        SourceDoc::note(
            "history",
            "# History\n\nGrace Okafor started Kestrel Robotics in 2004.\n",
        )
        .unwrap(),
    ];
    for i in 0..30 {
        docs.push(
            SourceDoc::note(
                &format!("noise{i}"),
                format!("# Note {i}\n\nThe company founded a club; the employer acquired chairs for project {i}.\n"),
            )
            .unwrap(),
        );
    }
    store.upsert(docs).unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let packet = engine
        .prepare(&PrepareRequest::new(
            "Who founded the company acquired by Bruno Moreau's employer?",
            600,
        ))
        .unwrap();
    let keys: Vec<&str> = packet.blocks.iter().map(|b| b.doc_key.as_str()).collect();
    for hop in ["note:staff", "note:deals", "note:history"] {
        assert!(keys.contains(&hop), "{hop} missing from {keys:?}");
    }
    drop(engine);
    drop(store);
}
