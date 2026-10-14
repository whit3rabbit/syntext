//! Conversation memory survives crashes: a torn journal tail is cut off,
//! every complete turn comes back, and old turns stay retrievable.

use std::io::Write;
use std::sync::Arc;

use syntext_context::{
    ChatRequest, ContextEngine, ContextStore, ConversationMemory, MemoryMode, Role, StoreOptions,
};

fn opts() -> StoreOptions {
    StoreOptions {
        sync: true,
        memory_flush_turns: 8,
        ..StoreOptions::default()
    }
}

#[test]
fn turns_survive_a_crash_with_a_torn_journal_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mode = MemoryMode::Project("billing".into());
    {
        let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
        let memory = ConversationMemory::new(Arc::clone(&store), "s1", mode.clone()).unwrap();
        memory
            .record_exchange("The refund window is 45 days.", "Got it: 45 days.")
            .unwrap();
        // Enough later conversation that the first turn no longer fits in
        // the history share of a 1K window.
        for i in 0..10 {
            memory
                .record_exchange(
                    &format!("Can you summarize section {i} of the onboarding guide for me?"),
                    &format!(
                        "Section {i} covers laptop setup, badge pickup, and the first-week \
                         schedule, including who to ask about access to shared drives."
                    ),
                )
                .unwrap();
        }
        // Crash: no flush, no clean shutdown.
        drop(memory);
        drop(store);
    }
    // A write was in flight when the process died.
    let journal = dir.path().join("journal.log");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap();
    f.write_all(&[0xFF, 0x00, 0x00, 0x00, 0x13, 0x37]).unwrap();
    drop(f);

    let store = Arc::new(ContextStore::open(dir.path(), opts()).unwrap());
    let memory = ConversationMemory::resume(Arc::clone(&store), "s1", mode).unwrap();
    let history = memory.history();
    assert_eq!(history.len(), 22, "every complete turn is back");
    assert_eq!(history[0].content, "The refund window is 45 days.");
    assert_eq!(history[0].role, Role::User);

    // New turns append after the repaired tail and survive a restart.
    memory.record(Role::User, "And exchanges?").unwrap();
    drop(memory);
    drop(store);
    let store = Arc::new(ContextStore::open(dir.path(), opts()).unwrap());
    let memory = ConversationMemory::resume(
        Arc::clone(&store),
        "s1",
        MemoryMode::Project("billing".into()),
    )
    .unwrap();
    assert_eq!(memory.history().len(), 23);

    // The earliest turn left the prompt long ago but is recalled on demand.
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let turn = engine
        .assemble(
            &ChatRequest::new("What did I say earlier about the refund window?", 1024)
                .memory(&memory),
        )
        .unwrap();
    assert!(turn.history_dropped > 0);
    assert!(turn.packet.evidence_text.contains("45 days"));
    drop(engine);
    drop(memory);
    drop(store);
}

#[test]
fn sessions_and_scopes_stay_separate() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    let a =
        ConversationMemory::new(Arc::clone(&store), "a", MemoryMode::App("alpha".into())).unwrap();
    let b =
        ConversationMemory::new(Arc::clone(&store), "b", MemoryMode::App("beta".into())).unwrap();
    a.record(Role::User, "Alpha's launch code word is marigold.")
        .unwrap();
    b.record(Role::User, "Beta prefers short answers.").unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let from_b = engine
        .assemble(&ChatRequest::new("What is the launch code word?", 2048).memory(&b))
        .unwrap();
    assert!(
        !from_b.packet.evidence_text.contains("marigold"),
        "app scopes isolate memory"
    );
    let resumed =
        ConversationMemory::resume(Arc::clone(&store), "a", MemoryMode::App("alpha".into()))
            .unwrap();
    assert_eq!(resumed.history().len(), 1);
    drop(engine);
    drop(store);
}

/// Every byte under `dir`, concatenated (segments, journal, manifest).
///
/// `write.lock` is skipped. It is never written, and on Windows the open
/// store's lock on it is a mandatory byte-range lock, so reading it fails
/// with `ERROR_LOCK_VIOLATION` (33) while the store is open.
fn store_bytes(dir: &std::path::Path) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "write.lock") {
                assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
            } else {
                out.extend(std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[test]
fn forget_removes_journaled_and_compacted_turns_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    let doomed =
        ConversationMemory::new(Arc::clone(&store), "doomed", MemoryMode::App("chat".into()))
            .unwrap();
    let other_session =
        ConversationMemory::new(Arc::clone(&store), "kept", MemoryMode::App("chat".into()))
            .unwrap();
    let same_session_other_scope = ConversationMemory::new(
        Arc::clone(&store),
        "doomed",
        MemoryMode::App("other".into()),
    )
    .unwrap();
    // Ten turns with flushes every eight: some compacted, some journaled.
    for i in 0..5 {
        doomed
            .record_exchange(
                &format!("Secret question {i}: the vault code is zebracorn{i}."),
                &format!("Noted zebracorn{i}."),
            )
            .unwrap();
    }
    other_session
        .record(Role::User, "The kept session mentions pelicanfish.")
        .unwrap();
    same_session_other_scope
        .record(Role::User, "Another scope mentions walrusberry.")
        .unwrap();
    assert!(
        !store.live_turns().is_empty(),
        "some turns are still journaled"
    );
    assert!(contains(&store_bytes(dir.path()), "zebracorn0"));

    assert_eq!(doomed.forget().unwrap(), 10);
    assert!(doomed.history().is_empty());
    let resumed =
        ConversationMemory::resume(Arc::clone(&store), "doomed", MemoryMode::App("chat".into()))
            .unwrap();
    assert!(
        resumed.history().is_empty(),
        "nothing of the session is left"
    );
    // Retrieval no longer sees it, even with its scope granted.
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let turn = engine
        .assemble(&ChatRequest::new("What is the vault code?", 2048).memory(&resumed))
        .unwrap();
    assert!(!turn.packet.evidence_text.contains("zebracorn"));
    drop(engine);

    // Idempotent.
    assert_eq!(doomed.forget().unwrap(), 0);

    // After a compaction no file holds the text, and the rest survived.
    store.compact().unwrap();
    let bytes = store_bytes(dir.path());
    for i in 0..5 {
        assert!(
            !contains(&bytes, &format!("zebracorn{i}")),
            "turn {i} still on disk after forget + compact"
        );
    }
    assert!(contains(&bytes, "pelicanfish"));
    assert!(contains(&bytes, "walrusberry"));

    // And across a reopen.
    drop(resumed);
    drop(doomed);
    drop(other_session);
    drop(same_session_other_scope);
    drop(store);
    let store = Arc::new(ContextStore::open(dir.path(), opts()).unwrap());
    let kept =
        ConversationMemory::resume(Arc::clone(&store), "kept", MemoryMode::App("chat".into()))
            .unwrap();
    assert_eq!(kept.history().len(), 1);
    let gone =
        ConversationMemory::resume(Arc::clone(&store), "doomed", MemoryMode::App("chat".into()))
            .unwrap();
    assert!(gone.history().is_empty());
    drop(kept);
    drop(gone);
    drop(store);
}

#[test]
fn forget_with_memory_off_only_clears_the_handle() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(ContextStore::create(dir.path(), opts()).unwrap());
    let memory = ConversationMemory::new(Arc::clone(&store), "s", MemoryMode::Off).unwrap();
    memory.record(Role::User, "ephemeral").unwrap();
    assert_eq!(memory.forget().unwrap(), 0);
    assert!(memory.history().is_empty());
    drop(memory);
    drop(store);
}
