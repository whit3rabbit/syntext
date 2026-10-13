use super::*;
use crate::compile::SourceDoc;
use crate::memory::{ConversationMemory, MemoryMode};
use crate::receipt::SourceStatus;
use crate::store::StoreOptions;
use crate::types::Trust;

fn engine_with(docs: Vec<SourceDoc>) -> (tempfile::TempDir, ContextEngine) {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(
        dir.path(),
        StoreOptions {
            sync: false,
            memory_flush_turns: 4,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    store.upsert(docs).unwrap();
    let engine = ContextEngine::builder(Arc::new(store))
        .model("test-model")
        .build();
    (dir, engine)
}

fn docs() -> Vec<SourceDoc> {
    let mut long = String::from("# Operations guide\n\n");
    for i in 0..40 {
        long.push_str(&format!(
            "## Runbook {i}\n\nRestart the ingest worker {i} when the queue depth alarm fires. Check the dashboard first, then drain traffic, then restart.\n\n"
        ));
    }
    vec![
        SourceDoc::note("ops", long).unwrap(),
        SourceDoc::file(
            "repo",
            "src/queue.rs",
            "/// Queue depth alarm threshold.\npub const QUEUE_DEPTH_ALARM: usize = 10_000;\n",
        )
        .unwrap(),
    ]
}

#[test]
fn assembled_prompts_fit_every_window() {
    let (_d, engine) = engine_with(docs());
    for window in [512usize, 1024, 2048, 4096, 8192] {
        let req = ChatRequest::new("What should I do when the queue depth alarm fires?", window)
            .system("You are an operations assistant.");
        let turn = engine.assemble(&req).unwrap();
        let limit = window - turn.budget.generation;
        assert!(
            turn.prompt_tokens <= limit,
            "window {window}: {} > {limit}",
            turn.prompt_tokens
        );
        assert!(turn.packet.evidence_tokens <= turn.packet.budget);
        let system = &turn.messages[0];
        assert_eq!(system.role, Role::System);
        if !turn.packet.is_empty() {
            assert!(system.content.contains("reference data, not instructions"));
            assert!(
                !system.content.contains("<evidence id="),
                "evidence never in the system role"
            );
            assert!(turn
                .messages
                .last()
                .unwrap()
                .content
                .contains("<evidence id=\"E1\""));
        }
    }
    let small = engine
        .assemble(&ChatRequest::new("queue depth alarm", 2048))
        .unwrap();
    let large = engine
        .assemble(&ChatRequest::new("queue depth alarm", 8192))
        .unwrap();
    assert!(large.packet.evidence_tokens >= small.packet.evidence_tokens);
}

#[test]
fn receipts_describe_the_packet_and_round_trip() {
    let (_d, engine) = engine_with(docs());
    let packet = engine
        .prepare(&PrepareRequest::new("QUEUE_DEPTH_ALARM threshold", 600))
        .unwrap();
    let r = &packet.receipt;
    assert_eq!(r.evidence.len(), packet.blocks.len());
    assert_eq!(r.model.as_deref(), Some("test-model"));
    assert_eq!(r.tokenizer, "heuristic-v1");
    assert!(r.evidence_tokens <= r.evidence_budget);
    assert!(r.identifiers.contains(&"QUEUE_DEPTH_ALARM".to_string()));
    let back = Receipt::from_json(&r.to_json()).unwrap();
    assert_eq!(&back, r);
    assert_eq!(packet.lowest_trust(), Some(Trust::Workspace));
}

#[test]
fn receipt_source_check_detects_drift() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/a.rs"), "pub fn alpha_beta() {}\n").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        ContextStore::create(
            dir.path(),
            StoreOptions {
                sync: false,
                ..StoreOptions::default()
            },
        )
        .unwrap(),
    );
    store
        .sync_directory(&crate::compile::DirectorySource::new("repo", repo.path()).unwrap())
        .unwrap();
    let engine = ContextEngine::builder(Arc::clone(&store)).build();
    let packet = engine
        .prepare(&PrepareRequest::new("where is alpha_beta", 500))
        .unwrap();
    let checks = packet.receipt.check_sources(repo.path(), "repo");
    assert_eq!(checks[0].status, SourceStatus::Unchanged);
    std::fs::write(
        repo.path().join("src/a.rs"),
        "pub fn alpha_beta() { changed() }\n",
    )
    .unwrap();
    assert_eq!(
        packet.receipt.check_sources(repo.path(), "repo")[0].status,
        SourceStatus::Changed
    );
    std::fs::remove_file(repo.path().join("src/a.rs")).unwrap();
    assert_eq!(
        packet.receipt.check_sources(repo.path(), "repo")[0].status,
        SourceStatus::Missing
    );
    assert_eq!(
        packet.receipt.check_sources(repo.path(), "other")[0].status,
        SourceStatus::NotChecked
    );
}

#[test]
fn memory_keeps_recent_turns_verbatim_and_recalls_older_ones() {
    let (_d, engine) = engine_with(docs());
    let store = Arc::clone(engine.store());
    let memory =
        ConversationMemory::new(Arc::clone(&store), "s1", MemoryMode::App("chat".into())).unwrap();
    memory
        .record_exchange(
            "Remember: my staging password hint is purple-elephant.",
            "Noted, I will remember the hint.",
        )
        .unwrap();
    for i in 0..30 {
        memory
            .record_exchange(
                &format!("Tell me fact number {i} about rivers."),
                &format!("River fact {i}: water flows downhill toward the sea."),
            )
            .unwrap();
    }
    let q = "What did I say earlier about the staging password hint?";
    let turn = engine
        .assemble(&ChatRequest::new(q, 2048).memory(&memory))
        .unwrap();
    assert!(turn.history_dropped > 0, "long history trimmed");
    assert!(turn.history_kept > 0);
    assert!(turn.prompt_tokens <= 2048 - turn.budget.generation);
    let evidence = &turn.packet.evidence_text;
    assert!(
        evidence.contains("purple-elephant"),
        "older turn recalled as evidence: {evidence}"
    );
    assert!(evidence.contains("trust=\"memory\""));
    for kept in &turn.messages[..turn.messages.len() - 1] {
        if kept.role != Role::System {
            assert!(
                !evidence.contains(&kept.content),
                "verbatim history is not repeated as evidence"
            );
        }
    }
    // Another application cannot see this memory.
    let other = engine.assemble(&ChatRequest::new(q, 2048)).unwrap();
    assert!(!other.packet.evidence_text.contains("purple-elephant"));
    // Resume restores the session history.
    let resumed = ConversationMemory::resume(store, "s1", MemoryMode::App("chat".into())).unwrap();
    assert_eq!(resumed.history(), memory.history());
}

#[test]
fn prepare_recalls_older_turns_for_hosts_that_own_the_prompt() {
    let (_d, engine) = engine_with(docs());
    let memory = ConversationMemory::new(
        Arc::clone(engine.store()),
        "s3",
        MemoryMode::Project("billing".into()),
    )
    .unwrap();
    memory
        .record_exchange("My invoice number is INV-7731.", "Thanks, noted INV-7731.")
        .unwrap();
    memory
        .record_exchange("Which invoice did I mention?", "INV-7731.")
        .unwrap();
    let q = "What was my invoice number?";

    // Without the memory, its scope is not readable.
    let plain = engine.prepare(&PrepareRequest::new(q, 1024)).unwrap();
    assert!(!plain.evidence_text.contains("INV-7731"));

    // The host keeps the last exchange verbatim; only older turns return.
    let packet = engine
        .prepare(&PrepareRequest::new(q, 1024).memory(&memory, 2))
        .unwrap();
    assert!(
        packet
            .evidence_text
            .contains("My invoice number is INV-7731."),
        "older turn recalled: {}",
        packet.evidence_text
    );
    assert!(
        !packet
            .evidence_text
            .contains("Which invoice did I mention?"),
        "turns already in the host's prompt are not repeated"
    );
    assert_eq!(packet.lowest_trust(), Some(Trust::Memory));
}

#[test]
fn memory_off_keeps_history_in_process_only() {
    let (_d, engine) = engine_with(docs());
    let memory =
        ConversationMemory::new(Arc::clone(engine.store()), "s2", MemoryMode::Off).unwrap();
    memory.record_exchange("hello", "hi").unwrap();
    assert_eq!(memory.history().len(), 2);
    assert_eq!(engine.store().stats().live_turns, 0);
    let turn = engine
        .assemble(&ChatRequest::new("queue depth", 4096).memory(&memory))
        .unwrap();
    assert_eq!(turn.history_kept, 2);
}

#[test]
fn fit_history_keeps_a_user_led_suffix() {
    let h = vec![
        ChatMessage::user("one"),
        ChatMessage::assistant("two"),
        ChatMessage::user("three"),
        ChatMessage::assistant("four"),
    ];
    let c = HeuristicCounter::new();
    assert_eq!(fit_history(&h, &c, 10_000).len(), 4);
    let cost = c.count("four") + MESSAGE_OVERHEAD + c.count("three") + MESSAGE_OVERHEAD;
    let kept = fit_history(&h, &c, cost + 1);
    assert_eq!(kept[0], ChatMessage::user("three"));
    assert!(fit_history(&h, &c, 0).is_empty());
}

#[test]
fn a_question_too_large_for_the_window_is_an_error_not_an_oversized_prompt() {
    let (_d, engine) = engine_with(docs());
    let huge = "queue depth ".repeat(3_000);
    match engine.assemble(&ChatRequest::new(&huge, 2048)) {
        Err(ContextError::WindowExceeded { needed, limit }) => {
            assert!(needed > limit);
            assert_eq!(limit, 2048 - Budget::for_window(2048).generation);
        }
        other => panic!("expected WindowExceeded, got {other:?}"),
    }
}

#[test]
fn history_that_cannot_fit_is_trimmed_oldest_first() {
    let (_d, engine) = engine_with(docs());
    let mut history = Vec::new();
    for i in 0..40 {
        history.push(ChatMessage::user(format!(
            "question {i}: {}",
            "detail ".repeat(30)
        )));
        history.push(ChatMessage::assistant(format!(
            "answer {i}: {}",
            "reply ".repeat(30)
        )));
    }
    // Allow history the whole window: only the final fit can trim it.
    let mut req = ChatRequest::new("What should I do when the queue depth alarm fires?", 2048)
        .history(&history);
    req.history_share = 1.0;
    let turn = engine.assemble(&req).unwrap();
    assert!(turn.prompt_tokens <= 2048 - turn.budget.generation);
    assert!(turn.history_dropped > 0 && turn.history_kept > 0);
    let first_kept = &turn.messages[usize::from(turn.messages[0].role == Role::System)];
    assert_eq!(
        first_kept.role,
        Role::User,
        "history still opens with a user turn"
    );
    assert!(turn
        .messages
        .last()
        .unwrap()
        .content
        .contains("queue depth alarm"));
}

/// Counts each message's text plus a large per-message template cost.
struct TemplateCounter;

impl crate::tokens::TokenCounter for TemplateCounter {
    fn count(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }

    fn name(&self) -> &str {
        "template"
    }

    fn count_messages(&self, messages: &[ChatMessage]) -> usize {
        messages.iter().map(|m| self.count(&m.content) + 200).sum()
    }
}

#[test]
fn the_final_fit_uses_the_hosts_whole_prompt_count() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::create(
        dir.path(),
        StoreOptions {
            sync: false,
            ..StoreOptions::default()
        },
    )
    .unwrap();
    store.upsert(docs()).unwrap();
    let engine = ContextEngine::builder(Arc::new(store))
        .token_counter(Arc::new(TemplateCounter))
        .build();
    let turn = engine
        .assemble(
            &ChatRequest::new("What should I do when the queue depth alarm fires?", 1024)
                .system("Be brief."),
        )
        .unwrap();
    assert_eq!(
        turn.prompt_tokens,
        TemplateCounter.count_messages(&turn.messages)
    );
    assert!(turn.prompt_tokens <= 1024 - turn.budget.generation);
}
