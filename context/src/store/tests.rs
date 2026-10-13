use super::manifest::segment_file_name;
use super::reader::SegmentReader;
use super::writer::write_segment;
use super::*;
use crate::compile::{compile_all, SourceDoc};
use crate::types::{DocKey, Role, Scope};

fn opts() -> StoreOptions {
    StoreOptions {
        sync: false,
        ..StoreOptions::default()
    }
}

fn note(id: &str, text: &str) -> SourceDoc {
    SourceDoc::note(id, text).unwrap()
}

fn keys(store: &ContextStore) -> Vec<String> {
    store
        .snapshot()
        .documents()
        .into_iter()
        .map(|d| d.key)
        .collect()
}

#[test]
fn upsert_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    let r = store
        .upsert(vec![
            note("a", "# Alpha\n\nalpha text"),
            note("b", "beta text"),
        ])
        .unwrap();
    assert_eq!((r.upserted, r.generation), (2, 1));
    assert_eq!(keys(&store), ["note:a", "note:b"]);
    drop(store);
    let store = ContextStore::open(dir.path(), opts()).unwrap();
    assert_eq!(keys(&store), ["note:a", "note:b"]);
    let text = store.snapshot().document_text("note:a").unwrap().unwrap();
    assert!(text.contains("alpha text"));
    drop(store);
}

#[test]
fn replacing_and_removing_documents() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store.upsert(vec![note("a", "first version")]).unwrap();
    store.upsert(vec![note("a", "second version")]).unwrap();
    let snap = store.snapshot();
    assert_eq!(snap.document_count(), 1);
    assert_eq!(
        snap.document_text("note:a").unwrap().unwrap(),
        "second version"
    );
    assert_eq!(snap.dead_count(), 1);
    let r = store
        .remove(&[
            DocKey::new("note:a").unwrap(),
            DocKey::new("note:zz").unwrap(),
        ])
        .unwrap();
    assert_eq!(r.removed, 1, "unknown keys are ignored");
    assert!(keys(&store).is_empty());
    drop(store);
    let store = ContextStore::open(dir.path(), opts()).unwrap();
    assert!(keys(&store).is_empty(), "removal is durable");
    drop(store);
}

#[test]
fn no_op_writes_do_not_bump_the_generation() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    let r = store.remove(&[DocKey::new("note:none").unwrap()]).unwrap();
    assert_eq!(r.generation, 0);
    assert_eq!(store.stats().generation, 0);
    drop(store);
}

#[test]
fn skipped_documents_remove_their_previous_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store.upsert(vec![note("a", "text")]).unwrap();
    let r = store.upsert(vec![note("a", "now\0binary")]).unwrap();
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.removed, 1);
    assert!(keys(&store).is_empty());
    drop(store);
}

#[test]
fn second_writer_is_locked_out_but_readers_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    assert!(matches!(
        ContextStore::open(dir.path(), opts()),
        Err(ContextError::Locked(_))
    ));
    let reader = ContextStore::open_read_only(dir.path(), opts()).unwrap();
    assert!(matches!(
        reader.upsert(vec![note("x", "y")]),
        Err(ContextError::ReadOnly)
    ));
    store.upsert(vec![note("a", "alpha")]).unwrap();
    assert!(reader.reload().unwrap());
    assert_eq!(keys(&reader), ["note:a"]);
    assert!(!reader.reload().unwrap(), "no change");
    drop(reader);
    drop(store);
}

#[test]
fn create_refuses_an_existing_store_and_open_requires_one() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        ContextStore::open(dir.path().join("missing"), opts()),
        Err(ContextError::NotFound(_))
    ));
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    drop(store);
    assert!(ContextStore::create(dir.path(), opts()).is_err());
    let store = ContextStore::open_or_create(dir.path(), opts()).unwrap();
    drop(store);
}

fn only_segment(dir: &Path) -> PathBuf {
    let segs: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ctxs"))
        .collect();
    assert_eq!(segs.len(), 1);
    segs[0].clone()
}

#[test]
fn metadata_corruption_fails_open_and_text_corruption_fails_verify() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store
        .upsert(vec![note("a", &"needle in a haystack ".repeat(20))])
        .unwrap();
    drop(store);
    let seg = only_segment(dir.path());
    let original = std::fs::read(&seg).unwrap();

    // Flip a byte inside the stored text: open is lazy, verify is not.
    let pos = original.windows(6).position(|w| w == b"needle").unwrap();
    let mut bytes = original.clone();
    bytes[pos] ^= 0x20;
    std::fs::write(&seg, &bytes).unwrap();
    let store = ContextStore::open(dir.path(), opts()).unwrap();
    assert!(matches!(store.verify(), Err(ContextError::Corrupt(_))));
    drop(store);

    // Flip a byte in the header: open fails.
    let mut bytes = original.clone();
    bytes[2] ^= 0xFF;
    std::fs::write(&seg, &bytes).unwrap();
    assert!(matches!(
        ContextStore::open(dir.path(), opts()),
        Err(ContextError::Corrupt(_))
    ));

    // Truncate: open fails.
    std::fs::write(&seg, &original[..original.len() / 2]).unwrap();
    assert!(ContextStore::open(dir.path(), opts()).is_err());
}

#[test]
fn every_single_byte_flip_in_metadata_is_rejected_or_harmless() {
    let docs = vec![
        note("a", "# T\n\nAlice works for Meridian Labs.\n"),
        note("b", "fn x() { y() }"),
    ];
    let (compiled, _) = compile_all(docs, &CompileOptions::default());
    let (bytes, _) = write_segment(&compiled, crate::graph::GraphMode::Full, 0).unwrap();
    for i in 0..bytes.len() {
        let mut b = bytes.clone();
        b[i] ^= 0x41;
        // Must never panic; most flips are caught by checksums.
        if let Ok(r) = SegmentReader::from_bytes(Arc::from(b)) {
            for t in 0..r.term_count() {
                let _ = r.postings(t);
            }
            for p in 0..r.passages.len() as u32 {
                let _ = r.passage_text(p);
            }
        }
    }
}

#[test]
fn compaction_merges_segments_and_drops_dead_documents() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    for i in 0..5 {
        store
            .upsert(vec![
                note("same", &format!("version {i}")),
                note(&format!("d{i}"), "x"),
            ])
            .unwrap();
    }
    let before = store.stats();
    assert_eq!(before.segments, 5);
    assert_eq!(before.dead_documents, 4);
    let report = store.maybe_compact().unwrap().expect("dead ratio exceeded");
    assert_eq!(report.segments_after, 1);
    assert_eq!(report.dead_dropped, 4);
    let after = store.stats();
    assert_eq!(
        (after.segments, after.dead_documents, after.documents),
        (1, 0, 6)
    );
    assert_eq!(
        store
            .snapshot()
            .document_text("note:same")
            .unwrap()
            .unwrap(),
        "version 4"
    );
    let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(
        files
            .iter()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "ctxs"))
            .count(),
        1,
        "superseded segments are garbage collected"
    );
    assert!(store.maybe_compact().unwrap().is_none());
    drop(store);
}

#[test]
fn turns_are_searchable_immediately_and_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let scope = Scope::project("demo").unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    let t1 = store
        .append_turn(
            "s1",
            Role::User,
            scope.clone(),
            "the deploy key rotates weekly",
            None,
        )
        .unwrap();
    let t2 = store
        .append_turn("s1", Role::Assistant, scope.clone(), "noted", None)
        .unwrap();
    assert_eq!((t1.seq, t2.seq), (1, 2));
    let doc = store.snapshot().document("turn:1").unwrap();
    assert_eq!(doc.scope, "project:demo");
    assert_eq!(doc.trust, crate::types::Trust::Memory);
    drop(store);
    // Restart: the live turns come back from the journal.
    let store = ContextStore::open(dir.path(), opts()).unwrap();
    assert_eq!(store.live_turns().len(), 2);
    assert!(store.snapshot().document("turn:2").is_some());
    assert!(store.flush_memory().unwrap());
    assert!(store.live_turns().is_empty());
    let t3 = store
        .append_turn("s1", Role::User, scope, "more", None)
        .unwrap();
    assert_eq!(t3.seq, 3, "sequence continues after compaction");
    drop(store);
    let store = ContextStore::open(dir.path(), opts()).unwrap();
    assert_eq!(keys(&store), ["turn:1", "turn:2", "turn:3"]);
    assert_eq!(store.live_turns().len(), 1);
    drop(store);
}

#[test]
fn memory_flushes_automatically_at_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(
        dir.path(),
        StoreOptions {
            memory_flush_turns: 3,
            ..opts()
        },
    )
    .unwrap();
    for i in 0..7 {
        store
            .append_turn(
                "s",
                Role::User,
                Scope::default(),
                &format!("message {i}"),
                Some(i),
            )
            .unwrap();
    }
    let stats = store.stats();
    assert_eq!(stats.live_turns, 1);
    assert_eq!(stats.segments, 2);
    assert_eq!(stats.documents, 7);
    drop(store);
}

#[test]
fn orphan_segments_and_temp_files_are_collected_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store.upsert(vec![note("a", "x")]).unwrap();
    drop(store);
    std::fs::write(dir.path().join(segment_file_name(99)), b"orphan").unwrap();
    std::fs::write(dir.path().join("manifest-crash.tmp"), b"partial").unwrap();
    let store = ContextStore::open(dir.path(), opts()).unwrap();
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
    assert_eq!(names.iter().filter(|n| n.ends_with(".ctxs")).count(), 1);
    assert_eq!(keys(&store), ["note:a"]);
    drop(store);
}

#[test]
fn duplicate_keys_in_one_batch_keep_the_last() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    let r = store
        .upsert(vec![note("a", "one"), note("a", "two")])
        .unwrap();
    assert_eq!(r.upserted, 1);
    assert_eq!(
        store.snapshot().document_text("note:a").unwrap().unwrap(),
        "two"
    );
    drop(store);
}

/// Rewrite a segment file's META section, re-checksumming everything.
fn rewrite_meta(path: &Path, mut f: impl FnMut(&mut serde_json::Value)) {
    use super::format::{SectionKind, FOOTER_LEN, TOC_ENTRY_LEN};
    let bytes = std::fs::read(path).unwrap();
    let footer = &bytes[bytes.len() - FOOTER_LEN..];
    let u64_at = |b: &[u8], i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let (toc_off, toc_len) = (u64_at(footer, 0) as usize, u64_at(footer, 8) as usize);
    let mut sections = Vec::new();
    for e in bytes[toc_off..toc_off + toc_len].chunks(TOC_ENTRY_LEN) {
        let kind = SectionKind::from_u32(u32::from_le_bytes(e[..4].try_into().unwrap())).unwrap();
        let (off, len) = (u64_at(e, 8) as usize, u64_at(e, 16) as usize);
        let mut body = bytes[off..off + len].to_vec();
        if kind == SectionKind::Meta {
            let mut meta: serde_json::Value = serde_json::from_slice(&body).unwrap();
            f(&mut meta);
            body = serde_json::to_vec(&meta).unwrap();
        }
        sections.push((kind, body));
    }
    std::fs::write(path, super::writer::assemble(sections, 0)).unwrap();
}

#[test]
fn segments_from_an_older_compiler_are_recompiled() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(dir.path(), opts()).unwrap();
    store
        .upsert(vec![
            note("a", "# Retries\n\nFailed charges are retried."),
            note("b", "x"),
        ])
        .unwrap();
    drop(store);
    rewrite_meta(&only_segment(dir.path()), |m| m["compiler"] = 0.into());

    let store = ContextStore::open(dir.path(), opts()).unwrap();
    assert_eq!(store.stats().stale_segments, 1);
    let report = store.maybe_compact().unwrap().expect("stale segment");
    assert_eq!(report.documents, 2);
    let stats = store.stats();
    assert_eq!((stats.stale_segments, stats.documents), (0, 2));
    assert_eq!(
        store.snapshot().document_text("note:a").unwrap().unwrap(),
        "# Retries\n\nFailed charges are retried."
    );
    assert!(store.maybe_compact().unwrap().is_none());
    drop(store);
}

/// Every TOC entry of a segment image as `(kind, flags, bytes)`.
fn raw_sections(bytes: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    use super::format::{FOOTER_LEN, TOC_ENTRY_LEN};
    let footer = &bytes[bytes.len() - FOOTER_LEN..];
    let u64_at = |b: &[u8], i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let (toc_off, toc_len) = (u64_at(footer, 0) as usize, u64_at(footer, 8) as usize);
    bytes[toc_off..toc_off + toc_len]
        .chunks(TOC_ENTRY_LEN)
        .map(|e| {
            let (off, len) = (u64_at(e, 8) as usize, u64_at(e, 16) as usize);
            (u32_at(e, 0), u32_at(e, 4), bytes[off..off + len].to_vec())
        })
        .collect()
}

#[test]
fn unknown_sections_are_skipped_only_when_marked_optional() {
    use super::format::SECTION_OPTIONAL;
    let (compiled, _) = compile_all(
        vec![note("a", "# T\n\nAlice works for Meridian Labs.\n")],
        &CompileOptions::default(),
    );
    let (bytes, _) = write_segment(&compiled, crate::graph::GraphMode::Full, 0).unwrap();
    let with_extra = |flags: u32| {
        let mut sections = raw_sections(&bytes);
        sections.push((200, flags, b"from a newer writer".to_vec()));
        super::writer::assemble_raw(sections, 0)
    };

    let r = SegmentReader::from_bytes(Arc::from(with_extra(SECTION_OPTIONAL))).unwrap();
    assert_eq!(r.docs.len(), 1);
    assert!(r.passage_text(0).unwrap().contains("Meridian Labs"));

    let err = SegmentReader::from_bytes(Arc::from(with_extra(0))).unwrap_err();
    assert!(
        err.to_string().contains("unknown required section"),
        "{err}"
    );
}
