//! The C ABI (`ffi` feature): call the `extern "C"` entry points as a
//! foreign host would and check the JSON and error contracts documented in
//! `swift/Sources/CSyntext/include/syntext_context.h`.

use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::ptr::{null, null_mut};

use serde_json::Value;
use syntext::ffi::{
    syntext_error, syntext_error_code, syntext_error_free, syntext_error_message,
    syntext_string_free, SYNTEXT_ERR_INVALID_ARGUMENT,
};
use syntext_context::ffi::*;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// Drain an error handle into `(code, message)`.
fn take_error(err: *mut syntext_error) -> Option<(u32, String)> {
    if err.is_null() {
        return None;
    }
    let code = syntext_error_code(err);
    let msg = unsafe { CStr::from_ptr(syntext_error_message(err)) }
        .to_string_lossy()
        .into_owned();
    syntext_error_free(err);
    Some((code, msg))
}

/// Parse an owned JSON result, panicking with the error when it is NULL.
fn json(ptr: *mut c_char, err: *mut syntext_error) -> Value {
    if ptr.is_null() {
        panic!("call failed: {:?}", take_error(err));
    }
    assert!(err.is_null(), "no error on success");
    let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    syntext_string_free(ptr);
    serde_json::from_str(&s).unwrap()
}

fn expect_error(ptr: *mut c_char, err: *mut syntext_error, code: u32) -> String {
    assert!(ptr.is_null(), "expected failure");
    let (got, msg) = take_error(err).expect("error handle");
    assert_eq!(got, code, "{msg}");
    msg
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn open_store(dir: &Path) -> *mut syntext_context_store {
    let mut err = null_mut();
    let store = syntext_context_store_open(
        c(dir.to_str().unwrap()).as_ptr(),
        c(r#"{"sync": false}"#).as_ptr(),
        &mut err,
    );
    assert!(!store.is_null(), "{:?}", take_error(err));
    store
}

fn engine(store: *const syntext_context_store) -> *mut syntext_context_engine {
    let mut err = null_mut();
    let e = syntext_context_engine_new(store, null(), None, null_mut(), null(), &mut err);
    assert!(!e.is_null(), "{:?}", take_error(err));
    e
}

fn source_json(root: &Path) -> CString {
    c(&serde_json::json!({"label": "repo", "root": root}).to_string())
}

#[test]
fn sync_prepare_search_and_check_receipt() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "src/retry.rs", "/// Retry a failed charge.\npub fn should_retry(attempt: u32) -> bool {\n    attempt < 3\n}\n");
    write(repo.path(), "README.md", "# Billing\n\nFailed charges are retried three times, unless the card was reported stolen.\n");
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());

    let mut err = null_mut();
    let report = json(
        syntext_context_store_sync(
            store,
            source_json(repo.path()).as_ptr(),
            None,
            null_mut(),
            &mut err,
        ),
        err,
    );
    assert_eq!(report["added"], 2);

    let engine = engine(store);
    let req = c(r#"{"question": "Are failed charges retried?", "evidence_tokens": 600}"#);
    let mut err = null_mut();
    let packet = json(
        syntext_context_prepare(engine, null(), req.as_ptr(), &mut err),
        err,
    );
    assert!(packet["evidence_text"]
        .as_str()
        .unwrap()
        .contains("reported stolen"));
    assert_eq!(packet["lowest_trust"], "workspace");
    let receipt_json = packet["receipt_json"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(receipt_json).unwrap(),
        packet["receipt"]
    );

    let req = c(r#"{"question": "where is should_retry defined", "max_results": 3}"#);
    let mut err = null_mut();
    let found = json(syntext_context_search(engine, req.as_ptr(), &mut err), err);
    assert_eq!(found[0]["source"], "src/retry.rs");
    assert_eq!(found[0]["symbol"], "should_retry");

    write(repo.path(), "README.md", "# Billing\n\nRewritten.\n");
    let mut err = null_mut();
    let checks = json(
        syntext_context_receipt_check(
            c(receipt_json).as_ptr(),
            c(repo.path().to_str().unwrap()).as_ptr(),
            c("repo").as_ptr(),
            &mut err,
        ),
        err,
    );
    let statuses: Vec<&str> = checks
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["status"].as_str().unwrap())
        .collect();
    assert!(statuses.contains(&"changed"), "{statuses:?}");

    syntext_context_engine_free(engine);
    syntext_context_store_free(store);
}

struct Progress {
    calls: u32,
    cancel: bool,
}

unsafe extern "C" fn on_progress(
    ud: *mut c_void,
    observed: u64,
    total: u64,
    _changed: u64,
) -> bool {
    let p = &mut *(ud as *mut Progress);
    assert!(observed <= total);
    p.calls += 1;
    !p.cancel
}

#[test]
fn sync_progress_can_cancel() {
    let repo = tempfile::tempdir().unwrap();
    for i in 0..10 {
        write(repo.path(), &format!("n{i}.md"), "note\n");
    }
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let mut state = Progress {
        calls: 0,
        cancel: true,
    };
    let mut err = null_mut();
    let out = syntext_context_store_sync(
        store,
        source_json(repo.path()).as_ptr(),
        Some(on_progress),
        (&mut state as *mut Progress).cast(),
        &mut err,
    );
    expect_error(out, err, SYNTEXT_CONTEXT_ERR_CANCELLED);
    assert_eq!(state.calls, 1);

    state.cancel = false;
    let mut err = null_mut();
    let report = json(
        syntext_context_store_sync(
            store,
            source_json(repo.path()).as_ptr(),
            Some(on_progress),
            (&mut state as *mut Progress).cast(),
            &mut err,
        ),
        err,
    );
    assert_eq!(report["added"], 10);
    syntext_context_store_free(store);
}

unsafe extern "C" fn count_words(_ud: *mut c_void, text: *const c_char, len: usize) -> u64 {
    let bytes = std::slice::from_raw_parts(text.cast::<u8>(), len);
    std::str::from_utf8(bytes)
        .unwrap()
        .split_whitespace()
        .count() as u64
}

#[test]
fn assemble_with_memory_and_a_host_token_counter() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let docs = c(r#"[{"id": "policy", "text": "Refunds are issued within 45 days of purchase."}]"#);
    let mut err = null_mut();
    let report = json(
        syntext_context_store_upsert(store, docs.as_ptr(), &mut err),
        err,
    );
    assert_eq!(report["upserted"], 1);

    let mut err = null_mut();
    let memory = syntext_context_memory_open(
        store,
        c("chat-1").as_ptr(),
        c(r#"{"project": "billing"}"#).as_ptr(),
        &mut err,
    );
    assert!(!memory.is_null(), "{:?}", take_error(err));
    for (role, text) in [
        ("user", "I bought it on March 3rd."),
        ("assistant", "Noted."),
    ] {
        let mut err = null_mut();
        let rc =
            syntext_context_memory_record(memory, c(role).as_ptr(), c(text).as_ptr(), &mut err);
        assert_eq!(rc, 0, "{:?}", take_error(err));
    }
    let mut err = null_mut();
    let rc =
        syntext_context_memory_record(memory, c("narrator").as_ptr(), c("x").as_ptr(), &mut err);
    assert_eq!(
        rc as u32,
        SYNTEXT_ERR_INVALID_ARGUMENT,
        "{:?}",
        take_error(err)
    );

    let mut err = null_mut();
    let engine = syntext_context_engine_new(
        store,
        c(r#"{"tokenizer": "words", "model": "test-model"}"#).as_ptr(),
        Some(count_words),
        null_mut(),
        null(),
        &mut err,
    );
    assert!(!engine.is_null(), "{:?}", take_error(err));
    let req =
        c(r#"{"question": "When do refunds expire?", "window": 1024, "system": "Be brief."}"#);
    let mut err = null_mut();
    let turn = json(
        syntext_context_assemble(engine, memory, req.as_ptr(), &mut err),
        err,
    );
    let messages = turn["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(
        messages[1]["content"], "I bought it on March 3rd.",
        "history from memory"
    );
    let last = messages.last().unwrap()["content"].as_str().unwrap();
    assert!(last.contains("45 days") && last.ends_with("When do refunds expire?"));
    assert_eq!(turn["packet"]["receipt"]["tokenizer"], "words");
    assert_eq!(turn["packet"]["receipt"]["model"], "test-model");
    assert!(turn["prompt_tokens"].as_u64().unwrap() <= 1024);

    // A host that builds its own prompt recalls the conversation through
    // `prepare`, leaving out the turns it already keeps verbatim.
    let ask = |kept: u32| {
        let req = c(&serde_json::json!({
            "question": "When was it bought?",
            "evidence_tokens": 400,
            "kept_turns": kept,
        })
        .to_string());
        let mut err = null_mut();
        json(
            syntext_context_prepare(engine, memory, req.as_ptr(), &mut err),
            err,
        )
    };
    let recalled = ask(0);
    assert!(recalled["evidence_text"]
        .as_str()
        .unwrap()
        .contains("March 3rd"));
    assert_eq!(recalled["lowest_trust"], "memory");
    let kept = ask(2);
    assert!(!kept["evidence_text"]
        .as_str()
        .unwrap()
        .contains("March 3rd"));

    let huge = "word ".repeat(5_000);
    let req = c(&serde_json::json!({"question": huge, "window": 1024}).to_string());
    let mut err = null_mut();
    let out = syntext_context_assemble(engine, null(), req.as_ptr(), &mut err);
    expect_error(out, err, SYNTEXT_CONTEXT_ERR_WINDOW_EXCEEDED);

    let mut err = null_mut();
    let history = json(syntext_context_memory_history(memory, &mut err), err);
    assert_eq!(history.as_array().unwrap().len(), 2);

    // Forgetting deletes the conversation's turns from the store.
    let mut removed = 0u64;
    let mut err = null_mut();
    let rc = syntext_context_memory_forget(memory, &mut removed, &mut err);
    assert_eq!(rc, 0, "{:?}", take_error(err));
    assert_eq!(removed, 2);
    let mut err = null_mut();
    let history = json(syntext_context_memory_history(memory, &mut err), err);
    assert!(history.as_array().unwrap().is_empty());
    let forgotten = ask(0);
    assert!(!forgotten["evidence_text"]
        .as_str()
        .unwrap()
        .contains("March 3rd"));
    let mut err = null_mut();
    let rc = syntext_context_memory_forget(memory, null_mut(), &mut err);
    assert_eq!(rc, 0, "idempotent, NULL out-param: {:?}", take_error(err));
    let mut err = null_mut();
    let rc = syntext_context_memory_forget(null(), null_mut(), &mut err);
    assert_eq!(
        rc as u32,
        SYNTEXT_ERR_INVALID_ARGUMENT,
        "{:?}",
        take_error(err)
    );

    // Handles may be freed in any order.
    syntext_context_store_free(store);
    syntext_context_memory_free(memory);
    syntext_context_engine_free(engine);
}

#[test]
fn boundary_misuse_is_an_error_not_a_crash() {
    let mut err = null_mut();
    let out = syntext_context_prepare(null(), null(), c("{}").as_ptr(), &mut err);
    expect_error(out, err, SYNTEXT_ERR_INVALID_ARGUMENT);

    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let engine = engine(store);
    let mut err = null_mut();
    let out = syntext_context_prepare(engine, null(), c("not json").as_ptr(), &mut err);
    expect_error(out, err, SYNTEXT_ERR_INVALID_ARGUMENT);

    let missing = dir.path().join("nothing-here");
    let mut err = null_mut();
    let none = syntext_context_store_open(
        c(missing.to_str().unwrap()).as_ptr(),
        c(r#"{"must_exist": true}"#).as_ptr(),
        &mut err,
    );
    assert!(none.is_null());
    assert_eq!(take_error(err).unwrap().0, SYNTEXT_CONTEXT_ERR_NOT_FOUND);

    let mut err = null_mut();
    let stats = json(syntext_context_store_stats(store, &mut err), err);
    assert_eq!(stats["documents"], 0);
    let mut err = null_mut();
    let compact = json(syntext_context_store_compact(store, false, &mut err), err);
    assert!(compact.is_null(), "nothing due");

    syntext_context_engine_free(engine);
    syntext_context_store_free(store);
    syntext_context_engine_free(null_mut());
    syntext_context_store_free(null_mut());
    syntext_context_memory_free(null_mut());
    let v = unsafe { CStr::from_ptr(syntext_context_version()) };
    assert_eq!(v.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn a_host_can_keep_the_policy_in_a_constant_system_prompt() {
    let policy = unsafe { CStr::from_ptr(syntext_context_evidence_policy()) }
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(policy, syntext_context::EVIDENCE_POLICY);

    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let docs = c(r#"[{"id": "a", "text": "The ingest worker restarts nightly."}]"#);
    let mut err = null_mut();
    json(
        syntext_context_store_upsert(store, docs.as_ptr(), &mut err),
        err,
    );
    let mut err = null_mut();
    let engine = syntext_context_engine_new(
        store,
        c(r#"{"append_policy": false}"#).as_ptr(),
        None,
        null_mut(),
        null(),
        &mut err,
    );
    assert!(!engine.is_null(), "{:?}", take_error(err));
    let system = format!("You are helpful.\n\n{policy}");
    let req = c(&serde_json::json!({
        "question": "When does the ingest worker restart?",
        "window": 2048,
        "system": system,
    })
    .to_string());
    let mut err = null_mut();
    let turn = json(
        syntext_context_assemble(engine, null(), req.as_ptr(), &mut err),
        err,
    );
    assert_eq!(
        turn["messages"][0]["content"], system,
        "system prompt unchanged"
    );
    assert!(!turn["packet"]["blocks"].as_array().unwrap().is_empty());
    syntext_context_engine_free(engine);
    syntext_context_store_free(store);
}

#[test]
fn placement_and_order_options_reach_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let docs = c(r#"[{"id": "a", "text": "The ingest worker restarts nightly."}]"#);
    let mut err = null_mut();
    json(
        syntext_context_store_upsert(store, docs.as_ptr(), &mut err),
        err,
    );
    let mut err = null_mut();
    let engine = syntext_context_engine_new(
        store,
        c(r#"{"placement": "separate_message", "order": "document"}"#).as_ptr(),
        None,
        null_mut(),
        null(),
        &mut err,
    );
    assert!(!engine.is_null(), "{:?}", take_error(err));
    let question = "When does the ingest worker restart?";
    let req = c(&serde_json::json!({"question": question, "window": 2048}).to_string());
    let mut err = null_mut();
    let turn = json(
        syntext_context_assemble(engine, null(), req.as_ptr(), &mut err),
        err,
    );
    let users: Vec<&Value> = turn["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .collect();
    assert_eq!(
        users.len(),
        2,
        "evidence and question are separate messages"
    );
    assert!(users[0]["content"]
        .as_str()
        .unwrap()
        .contains("<evidence id=\"E1\""));
    assert_eq!(users[1]["content"], question);

    let mut err = null_mut();
    let bad = syntext_context_engine_new(
        store,
        c(r#"{"placement": "system"}"#).as_ptr(),
        None,
        null_mut(),
        null(),
        &mut err,
    );
    assert!(
        bad.is_null(),
        "evidence can never be placed in the system role"
    );
    let (code, _) = take_error(err).expect("error handle");
    assert_eq!(code, SYNTEXT_ERR_INVALID_ARGUMENT);
    syntext_context_engine_free(engine);
    syntext_context_store_free(store);
}

/// A toy model: "money" and "refunds" share a dimension, other words hash.
unsafe extern "C" fn embed_words(
    userdata: *mut c_void,
    texts: *const *const c_char,
    lens: *const usize,
    count: usize,
    _is_query: bool,
    out: *mut f32,
) -> bool {
    if !userdata.is_null() {
        return false; // the "failing model" variant
    }
    let dims = 16;
    for i in 0..count {
        let bytes = std::slice::from_raw_parts(*texts.add(i) as *const u8, *lens.add(i));
        let text = std::str::from_utf8(bytes).unwrap().to_lowercase();
        let row = std::slice::from_raw_parts_mut(out.add(i * dims), dims);
        row.fill(0.0);
        row[dims - 1] = 0.01;
        for w in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
        {
            let d = if w == "money" || w.starts_with("refund") {
                0
            } else {
                1 + w.bytes().map(usize::from).sum::<usize>() % (dims - 2)
            };
            row[d] += 1.0;
        }
    }
    true
}

#[test]
fn a_host_embedding_callback_powers_semantic_retrieval() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let docs = c(
        r#"[{"id": "refunds", "text": "Refunds are issued within 45 days."},
                     {"id": "menu", "text": "The cafeteria menu changes every Monday."}]"#,
    );
    let mut err = null_mut();
    json(
        syntext_context_store_upsert(store, docs.as_ptr(), &mut err),
        err,
    );

    let mut err = null_mut();
    let bad = syntext_context_embedder_new(
        c("toy").as_ptr(),
        0,
        Some(embed_words),
        null_mut(),
        &mut err,
    );
    assert!(bad.is_null());
    assert_eq!(take_error(err).unwrap().0, SYNTEXT_ERR_INVALID_ARGUMENT);
    let mut err = null_mut();
    let bad = syntext_context_embedder_new(c("toy").as_ptr(), 16, None, null_mut(), &mut err);
    assert!(bad.is_null());
    assert_eq!(take_error(err).unwrap().0, SYNTEXT_ERR_INVALID_ARGUMENT);

    let mut err = null_mut();
    let embedder = syntext_context_embedder_new(
        c("toy-v1").as_ptr(),
        16,
        Some(embed_words),
        null_mut(),
        &mut err,
    );
    assert!(!embedder.is_null(), "{:?}", take_error(err));
    let mut err = null_mut();
    let report = json(
        syntext_context_store_embed(store, embedder, null(), None, null_mut(), &mut err),
        err,
    );
    assert_eq!(report["model"], "toy-v1");
    assert_eq!(report["embedded"], 2);
    assert_eq!(report["remaining"], 0);

    let mut err = null_mut();
    let engine = syntext_context_engine_new(store, null(), None, null_mut(), embedder, &mut err);
    assert!(!engine.is_null(), "{:?}", take_error(err));
    // The engine keeps its own reference to the embedder.
    syntext_context_embedder_free(embedder);
    let req = c(r#"{"question": "How do I get my money back?", "evidence_tokens": 300}"#);
    let mut err = null_mut();
    let packet = json(
        syntext_context_prepare(engine, null(), req.as_ptr(), &mut err),
        err,
    );
    assert!(packet["evidence_text"]
        .as_str()
        .unwrap()
        .contains("Refunds are issued"));
    assert_eq!(packet["receipt"]["embedder"], "toy-v1");

    // A failing model is an error with its own code, not a crash.
    let mut err = null_mut();
    let failing = syntext_context_embedder_new(
        c("toy-v2").as_ptr(),
        16,
        Some(embed_words),
        std::ptr::dangling_mut::<c_void>(),
        &mut err,
    );
    let mut err = null_mut();
    let out = syntext_context_store_embed(store, failing, null(), None, null_mut(), &mut err);
    expect_error(out, err, SYNTEXT_CONTEXT_ERR_EMBEDDER);
    syntext_context_embedder_free(failing);
    syntext_context_engine_free(engine);
    syntext_context_store_free(store);
}

#[test]
fn agent_tools_through_the_c_abi() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "src/retry.rs", "/// Retry a failed charge.\npub fn should_retry(attempt: u32) -> bool {\n    attempt < 3\n}\n");
    write(repo.path(), "docs/billing.md", "# Billing\n\n## Retries\n\nFailed charges are retried three times.\n\n## Refunds\n\nRefunds take 45 days.\n");
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let mut err = null_mut();
    let src = source_json(repo.path());
    json(
        syntext_context_store_sync(store, src.as_ptr(), None, null_mut(), &mut err),
        err,
    );
    let engine = engine(store);
    let call = |request: &str| {
        let req = c(request);
        let mut err = null_mut();
        json(
            syntext_context_tool_call(engine, req.as_ptr(), &mut err),
            err,
        )
    };

    let mut err = null_mut();
    let openai = json(syntext_context_tool_definitions(null(), &mut err), err);
    assert_eq!(openai[0]["function"]["name"], "context_search");
    let mut err = null_mut();
    let mcp = json(
        syntext_context_tool_definitions(c("mcp").as_ptr(), &mut err),
        err,
    );
    assert_eq!(mcp[2]["name"], "context_outline");
    assert!(mcp[1]["inputSchema"]["properties"]["start_line"].is_object());
    let mut err = null_mut();
    let bad = syntext_context_tool_definitions(c("xml").as_ptr(), &mut err);
    expect_error(bad, err, SYNTEXT_ERR_INVALID_ARGUMENT);

    // Arguments exactly as the model wrote them; ids continue after E3.
    let found = call(
        r#"{"name": "context_search", "arguments": "{\"query\": \"refunds\"}", "first_id": 4}"#,
    );
    assert_eq!(found["is_error"], false);
    assert_eq!(found["blocks"][0]["id"], "E4");
    assert!(found["content"]
        .as_str()
        .unwrap()
        .contains("Refunds take 45 days"));
    let n = found["blocks"].as_array().unwrap().len();
    assert_eq!(found["next_id"], 4 + n);
    assert_eq!(found["lowest_trust"], "workspace");

    let outline =
        call(r#"{"name": "context_outline", "arguments": {"source": "docs/billing.md"}}"#);
    let text = outline["content"].as_str().unwrap();
    assert!(text.contains("  Retries (lines 3-6)"), "{text}");

    let read = call(
        r#"{"name": "context_read", "arguments": {"source": "src/retry.rs", "start_line": 2, "end_line": 2}}"#,
    );
    assert_eq!(read["blocks"][0]["lines"], serde_json::json!([2, 2]));

    // The model's mistakes come back as results; the scopes are the app's.
    let hidden = call(
        r#"{"name": "context_read", "arguments": {"source": "docs/billing.md"}, "scopes": ["app:other"]}"#,
    );
    assert_eq!(hidden["is_error"], true);
    let garbled = call(r#"{"name": "context_read", "arguments": "not json"}"#);
    assert_eq!(garbled["is_error"], true);

    // The host's mistakes are errors.
    let req = c(r#"{"name": "context_search", "arguments": {}, "result_tokens": 10}"#);
    let mut err = null_mut();
    let small = syntext_context_tool_call(engine, req.as_ptr(), &mut err);
    expect_error(small, err, SYNTEXT_ERR_INVALID_ARGUMENT);
    let mut err = null_mut();
    let none = syntext_context_tool_call(null(), req.as_ptr(), &mut err);
    expect_error(none, err, SYNTEXT_ERR_INVALID_ARGUMENT);

    syntext_context_engine_free(engine);
    syntext_context_store_free(store);
}
