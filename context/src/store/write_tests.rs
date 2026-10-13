use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use super::super::{ContextStore, StoreOptions};
use crate::compile::SourceDoc;
use crate::types::{Role, Scope};

fn opts(batch_bytes: usize) -> StoreOptions {
    StoreOptions {
        sync: false,
        batch_bytes,
        ..StoreOptions::default()
    }
}

fn note(id: &str, text: &str) -> SourceDoc {
    SourceDoc::note(id, text).unwrap()
}

fn segment_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".ctxs"))
        .collect();
    names.sort();
    names
}

#[test]
fn a_large_write_is_staged_in_batches_and_published_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts(64)).unwrap();
    let docs: Vec<SourceDoc> = (0..10)
        .map(|i| {
            note(
                &format!("d{i}"),
                &format!("document {i} {}", "word ".repeat(12)),
            )
        })
        .collect();
    let r = store.upsert(docs).unwrap();
    assert_eq!(r.upserted, 10);
    assert!(r.segments >= 5, "one segment per batch: {}", r.segments);
    assert_eq!(r.generation, 1, "one generation for the whole write");
    assert_eq!(store.snapshot().generation(), 1);
    let stats = store.stats();
    assert_eq!((stats.segments, stats.documents), (r.segments, 10));
    drop(store);

    let store = ContextStore::open(dir.path(), opts(64)).unwrap();
    assert_eq!(store.stats().documents, 10);
    assert_eq!(segment_files(dir.path()).len(), r.segments);
    drop(store);
}

#[test]
fn a_key_staged_twice_in_one_write_keeps_the_later_copy() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts(16)).unwrap();
    let r = store
        .write_batched(|s| {
            s.push(note("a", &"first ".repeat(8)))?;
            s.push(note("a", &"second ".repeat(8)))?;
            Ok(Vec::new())
        })
        .unwrap();
    assert_eq!((r.upserted, r.segments), (1, 2));
    let text = store.snapshot().document_text("note:a").unwrap().unwrap();
    assert!(text.starts_with("second"), "{text}");
    let stats = store.stats();
    assert_eq!((stats.documents, stats.dead_documents), (1, 1));
    drop(store);
}

#[test]
fn small_segments_merge_without_rewriting_full_ones() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(
        dir.path(),
        StoreOptions {
            max_segments: 2,
            ..opts(4096)
        },
    )
    .unwrap();
    store
        .upsert(vec![note("big", &"alpha beta gamma delta ".repeat(300))])
        .unwrap();
    let full = segment_files(dir.path());
    for i in 0..3 {
        store
            .upsert(vec![note(&format!("s{i}"), "a small note")])
            .unwrap();
    }
    assert_eq!(store.stats().segments, 4);

    let r = store
        .maybe_compact()
        .unwrap()
        .expect("three small segments");
    assert_eq!((r.segments_before, r.segments_after), (4, 2));
    assert_eq!(r.documents, 3, "only the small segments are rewritten");
    let after = segment_files(dir.path());
    assert!(after.contains(&full[0]), "the full segment is untouched");
    assert_eq!(after.len(), 2);
    assert_eq!(store.stats().documents, 4);
    assert!(store.maybe_compact().unwrap().is_none());

    // A full compaction rewrites everything, still a batch per segment:
    // the large note fills one batch alone, the small notes share another.
    let r = store.compact().unwrap();
    assert_eq!((r.documents, r.segments_after), (4, 2));
    assert!(!segment_files(dir.path()).contains(&full[0]));
    drop(store);
}

#[test]
fn recording_a_turn_never_waits_on_a_running_write() {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions {
        memory_flush_turns: 1,
        ..opts(1 << 20)
    };
    let store = Arc::new(ContextStore::create(dir.path(), opts).unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let writer = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            store
                .write_batched(|s| {
                    s.push(note("doc", "a document being written"))?;
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(Vec::new())
                })
                .unwrap()
        })
    };
    entered_rx.recv().unwrap();

    // Due for a flush (threshold 1), but the write holds the segment lock:
    // the turn must still be recorded, durable, and searchable right away.
    let (done_tx, done_rx) = mpsc::channel();
    {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            let turn = store
                .append_turn("s", Role::User, Scope::default(), "said mid-sync", None)
                .unwrap();
            done_tx.send(turn).unwrap();
        });
    }
    // Generous guard so a regression fails instead of hanging the suite.
    let turn = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("append_turn blocked on the running write");
    assert_eq!(store.live_turns().len(), 1, "flush deferred");
    let key = format!("turn:{}", turn.seq);
    assert!(store.snapshot().lookup(&key).is_some());

    release_tx.send(()).unwrap();
    writer.join().unwrap();
    assert!(
        store.snapshot().lookup(&key).is_some(),
        "the write kept the live turn"
    );
    store
        .append_turn("s", Role::Assistant, Scope::default(), "reply", None)
        .unwrap();
    assert_eq!(store.live_turns().len(), 0, "the next turn flushes both");
    assert_eq!(store.stats().documents, 3);
    drop(store);
}
