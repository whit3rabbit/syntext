//! Semantic retrieval through a host embedder: the embedding job, reuse of
//! identical text, compaction, model changes, read-only reload, and the
//! effect on retrieval.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use syntext_context::{
    ContextEngine, ContextError, ContextStore, DocKey, EmbedOptions, Embedder, PrepareRequest,
    SourceDoc, StoreOptions,
};

const DIMS: usize = 32;

/// Words that mean the same thing share a dimension, like a real model
/// placing paraphrases close together; other words hash to their own.
const CONCEPTS: &[&[&str]] = &[
    &["money", "refund", "refunds", "reimbursed"],
    &["puppy", "dogs", "dog"],
    &["login", "password", "sign"],
];

struct Toy {
    id: String,
    calls: AtomicUsize,
    texts: AtomicUsize,
}

impl Toy {
    fn new(id: &str) -> Self {
        Toy {
            id: id.to_string(),
            calls: AtomicUsize::new(0),
            texts: AtomicUsize::new(0),
        }
    }

    fn vector(text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; DIMS];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(str::to_lowercase)
        {
            let dim = match CONCEPTS.iter().position(|c| c.contains(&word.as_str())) {
                Some(i) => i,
                None => {
                    let h = word
                        .bytes()
                        .fold(7u32, |h, b| h.wrapping_mul(31) ^ u32::from(b));
                    CONCEPTS.len() + (h as usize % (DIMS - CONCEPTS.len()))
                }
            };
            v[dim] += 1.0;
        }
        v[DIMS - 1] += 0.01; // never all zero
        v
    }
}

impl Embedder for Toy {
    fn id(&self) -> &str {
        &self.id
    }

    fn dims(&self) -> usize {
        DIMS
    }

    fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.texts.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts.iter().map(|t| Toy::vector(t)).collect())
    }
}

fn opts() -> StoreOptions {
    StoreOptions {
        sync: false,
        ..StoreOptions::default()
    }
}

fn docs() -> Vec<SourceDoc> {
    vec![
        SourceDoc::note(
            "refunds",
            "# Refund policy\n\nRefunds are issued within 45 days of purchase.\n",
        )
        .unwrap(),
        SourceDoc::note("dogs", "# Animals\n\nDogs are welcome on Fridays.\n").unwrap(),
        SourceDoc::note("menu", "# Cafeteria\n\nThe menu changes every Monday.\n").unwrap(),
    ]
}

#[test]
fn a_paraphrase_finds_its_answer_once_passages_are_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    store.upsert(docs()).unwrap();
    let toy = Arc::new(Toy::new("toy-v1"));
    let engine = ContextEngine::builder(Arc::clone(&store))
        .embedder(toy.clone())
        .build();
    let question = "How do I get my money back?";

    // Nothing embedded yet: lexical retrieval alone, and no query embedding.
    let before = engine.prepare(&PrepareRequest::new(question, 400)).unwrap();
    assert!(!before.evidence_text.contains("Refunds are issued"));
    assert_eq!(toy.calls.load(Ordering::SeqCst), 0);

    let report = store.embed(toy.as_ref(), &EmbedOptions::default()).unwrap();
    assert_eq!(report.model, "toy-v1");
    assert_eq!((report.embedded, report.remaining), (3, 0));
    assert_eq!(report.covered, 3);

    let after = engine.prepare(&PrepareRequest::new(question, 400)).unwrap();
    assert!(
        after.evidence_text.contains("Refunds are issued"),
        "{}",
        after.evidence_text
    );
    assert!(after.blocks[0].signals.semantic > 0.0);
    assert_eq!(after.receipt.embedder.as_deref(), Some("toy-v1"));
    drop(engine);
    drop(store);
}

#[test]
fn identical_text_is_embedded_once_across_syncs_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store.upsert(docs()).unwrap();
    let toy = Toy::new("toy-v1");
    store.embed(&toy, &EmbedOptions::default()).unwrap();
    assert_eq!(toy.texts.load(Ordering::SeqCst), 3);

    // Nothing new: no calls.
    let again = store.embed(&toy, &EmbedOptions::default()).unwrap();
    assert_eq!((again.embedded, again.covered), (0, 3));

    // Re-upserting the same text writes a new segment; its vectors are
    // copied, not recomputed. Changed text is embedded.
    store.upsert(docs()).unwrap();
    store
        .upsert(vec![SourceDoc::note(
            "menu",
            "# Cafeteria\n\nTacos on Tuesday.\n",
        )
        .unwrap()])
        .unwrap();
    let r = store.embed(&toy, &EmbedOptions::default()).unwrap();
    assert_eq!((r.embedded, r.reused, r.remaining), (1, 2, 0), "{r:?}");
    assert_eq!(toy.texts.load(Ordering::SeqCst), 4);

    // Compaction rewrites every segment and carries vectors with them.
    store.compact().unwrap();
    let after = store.embed(&toy, &EmbedOptions::default()).unwrap();
    assert_eq!(
        (after.embedded, after.reused, after.covered),
        (0, 0, 3),
        "{after:?}"
    );
    let vec_files = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".vec")
        })
        .count();
    assert_eq!(
        vec_files,
        store.stats().segments,
        "one sidecar per segment, no orphans"
    );
    drop(store);
}

#[test]
fn a_new_model_re_embeds_and_replaces_the_old_vectors() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store.upsert(docs()).unwrap();
    store
        .embed(&Toy::new("toy-v1"), &EmbedOptions::default())
        .unwrap();
    let v2 = Toy::new("toy-v2");
    let r = store.embed(&v2, &EmbedOptions::default()).unwrap();
    assert_eq!(r.embedded, 3, "vectors of another model are never reused");
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".vec"))
        .collect();
    assert_eq!(names.len(), 1, "old model pruned: {names:?}");
    drop(store);
}

#[test]
fn a_budget_or_cancel_stops_early_and_the_next_run_continues() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store.upsert(docs()).unwrap();
    let toy = Toy::new("toy-v1");
    let first = store
        .embed(
            &toy,
            &EmbedOptions {
                max_passages: Some(1),
                ..EmbedOptions::default()
            },
        )
        .unwrap();
    assert_eq!((first.embedded, first.remaining), (1, 2));
    let small_batches = EmbedOptions {
        batch: 1,
        ..EmbedOptions::default()
    };
    let cancelled = store.embed_with(&toy, &small_batches, |_| false).unwrap();
    assert!(cancelled.cancelled);
    assert_eq!((cancelled.embedded, cancelled.remaining), (1, 1));
    let rest = store.embed(&toy, &EmbedOptions::default()).unwrap();
    assert_eq!((rest.embedded, rest.remaining, rest.covered), (1, 0, 3));
    drop(store);
}

#[test]
fn bad_vectors_are_an_error_and_damaged_sidecars_are_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store.upsert(docs()).unwrap();
    let wrong = syntext_context::FnEmbedder::new("short", 4, |texts: &[&str], _| {
        Ok(texts.iter().map(|_| vec![1.0; 3]).collect())
    });
    let err = store.embed(&wrong, &EmbedOptions::default()).unwrap_err();
    assert!(matches!(err, ContextError::Embedder(_)), "{err}");

    let toy = Toy::new("toy-v1");
    store.embed(&toy, &EmbedOptions::default()).unwrap();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "vec") {
            let mut bytes = std::fs::read(&path).unwrap();
            let mid = bytes.len() / 2;
            bytes[mid] ^= 0xff;
            std::fs::write(&path, bytes).unwrap();
        }
    }
    // A fresh handle ignores the damaged file and the next run rebuilds it.
    drop(store);
    let store = ContextStore::open(dir.path(), opts()).unwrap();
    let r = store.embed(&toy, &EmbedOptions::default()).unwrap();
    assert_eq!((r.embedded, r.covered), (3, 3));
    drop(store);
}

#[test]
fn readers_pick_up_vectors_after_reload_and_cannot_embed() {
    let dir = tempfile::tempdir().unwrap();
    let writer = ContextStore::create(dir.path(), opts()).unwrap();
    writer.upsert(docs()).unwrap();
    let reader = Arc::new(ContextStore::open_read_only(dir.path(), opts()).unwrap());
    let toy = Arc::new(Toy::new("toy-v1"));
    assert!(matches!(
        reader.embed(toy.as_ref(), &EmbedOptions::default()),
        Err(ContextError::ReadOnly)
    ));
    let engine = ContextEngine::builder(Arc::clone(&reader))
        .embedder(toy.clone())
        .build();
    let q = PrepareRequest::new("How do I get my money back?", 400);
    assert!(!engine
        .prepare(&q)
        .unwrap()
        .evidence_text
        .contains("Refunds are issued"));
    writer
        .embed(toy.as_ref(), &EmbedOptions::default())
        .unwrap();
    reader.reload().unwrap();
    assert!(engine
        .prepare(&q)
        .unwrap()
        .evidence_text
        .contains("Refunds are issued"));
    // Removing a document removes it from semantic results too.
    writer
        .remove(&[DocKey::new("note:refunds").unwrap()])
        .unwrap();
    reader.reload().unwrap();
    assert!(!engine
        .prepare(&q)
        .unwrap()
        .evidence_text
        .contains("Refunds are issued"));
    drop(engine);
    drop(reader);
    drop(writer);
}
