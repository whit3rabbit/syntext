//! Building a store for a set and asking its questions.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use syntext_context::{
    syntext, ChatRequest, ContextEngine, ContextStore, ConversationMemory, DirectorySource,
    EmbedOptions, MemoryMode, Role, Scope, SourceDoc, StoreOptions, SyntextSource,
};

use super::score;
use super::{Corpus, EvalSet, Note, QuestionResult, RunOptions, SetOutcome};

/// Memory scope of conversation corpora.
const MEMORY_PROJECT: &str = "eval";
/// Left out of `repository` corpora: the set files contain the questions.
const EVAL_DIR: &str = "context/eval";

pub(super) fn run(set: &EvalSet, opts: &RunOptions) -> Result<SetOutcome, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let store = Arc::new(
        ContextStore::create(
            dir.path().join("store"),
            StoreOptions {
                sync: false,
                ..StoreOptions::default()
            },
        )
        .map_err(|e| e.to_string())?,
    );
    let mut index = None;
    let mut memory = None;
    match &set.corpus {
        Corpus::Notes { docs } => upsert_notes(&store, docs)?,
        Corpus::Repository {
            label,
            exclude,
            syntext,
        } => {
            // The sets themselves hold every question verbatim.
            let mut exclude = exclude.clone();
            exclude.push(EVAL_DIR.to_string());
            index = sync_dir(
                &store,
                dir.path(),
                label,
                &opts.repo_root,
                &exclude,
                *syntext,
            )?;
        }
        Corpus::External {
            name,
            label,
            exclude,
            syntext,
        } => {
            let Some(root) = opts.external.get(name) else {
                return Ok(SetOutcome::Skipped(format!("needs --corpus {name}=<dir>")));
            };
            index = sync_dir(&store, dir.path(), label, root, exclude, *syntext)?;
        }
        Corpus::Conversation { turns, docs } => {
            upsert_notes(&store, docs)?;
            let m = ConversationMemory::new(
                Arc::clone(&store),
                "eval",
                MemoryMode::Project(MEMORY_PROJECT.into()),
            )
            .map_err(|e| e.to_string())?;
            for t in turns {
                let role = match t.role.as_str() {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    other => return Err(format!("{}: unknown role {other}", set.name)),
                };
                m.record(role, &t.content).map_err(|e| e.to_string())?;
            }
            // Into durable segments, as a long conversation would be: turns
            // still in the live journal are never embedded.
            store.flush_memory().map_err(|e| e.to_string())?;
            memory = Some(m);
        }
    }
    let mut builder = ContextEngine::builder(Arc::clone(&store));
    if let Some((index, label)) = index {
        builder = builder.syntext(SyntextSource::new(index, &label).map_err(|e| e.to_string())?);
    }
    if let Some(e) = &opts.embedder {
        store
            .embed(e.as_ref(), &EmbedOptions::default())
            .map_err(|e| format!("{}: embedding: {e}", set.name))?;
        builder = builder.embedder(Arc::clone(e));
    }
    let engine = builder.build();

    let mut results = Vec::new();
    for q in &set.questions {
        for &window in &opts.windows {
            results.push(ask(&engine, memory.as_ref(), set, q, window, opts));
        }
    }
    drop(memory);
    drop(engine);
    drop(store);
    Ok(SetOutcome::Ran(results))
}

fn upsert_notes(store: &ContextStore, docs: &[Note]) -> Result<(), String> {
    if docs.is_empty() {
        return Ok(());
    }
    let docs = docs
        .iter()
        .map(|d| {
            let doc = SourceDoc::note(&d.id, d.text.clone()).map_err(|e| e.to_string())?;
            Ok(match &d.title {
                Some(t) => doc.with_title(t.clone()),
                None => doc,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    store.upsert(docs).map_err(|e| e.to_string())?;
    Ok(())
}

type Attached = Option<(Arc<syntext::index::Index>, String)>;

/// Sync `root` under `label`, optionally building a syntext index of it.
fn sync_dir(
    store: &ContextStore,
    scratch: &Path,
    label: &str,
    root: &Path,
    exclude: &[String],
    with_syntext: bool,
) -> Result<Attached, String> {
    let mut source = DirectorySource::new(label, root).map_err(|e| e.to_string())?;
    for prefix in exclude {
        source = source.exclude(prefix);
    }
    store.sync_directory(&source).map_err(|e| e.to_string())?;
    if !with_syntext {
        return Ok(None);
    }
    let config = syntext::Config::new(scratch.join("index"), root.to_path_buf());
    let index = syntext::index::Index::build(config).map_err(|e| e.to_string())?;
    Ok(Some((Arc::new(index), label.to_string())))
}

fn ask(
    engine: &ContextEngine,
    memory: Option<&ConversationMemory>,
    set: &EvalSet,
    q: &super::Question,
    window: usize,
    opts: &RunOptions,
) -> QuestionResult {
    let mut result = QuestionResult {
        set: set.name.clone(),
        id: q.id.clone(),
        window,
        tags: q.tags.clone(),
        supports: vec![false; q.supports.len()],
        first_rank: None,
        retrieved_all: false,
        evidence_tokens: 0,
        blocks: 0,
        prompt_tokens: 0,
        micros: 0,
        error: None,
    };
    let mut request = ChatRequest::new(&q.question, window).system(&opts.system);
    if let Some(m) = memory {
        request = request.memory(m);
    }
    let start = Instant::now();
    let turn = match engine.assemble(&request) {
        Ok(turn) => turn,
        Err(e) => {
            result.error = Some(e.to_string());
            return result;
        }
    };
    result.micros = start.elapsed().as_micros() as u64;
    result.evidence_tokens = turn.packet.evidence_tokens;
    result.blocks = turn.packet.blocks.len();
    result.prompt_tokens = turn.prompt_tokens;
    // Verbatim history the model also sees: every message but the system
    // prompt and the final one, whose evidence is scored block by block.
    let history: Vec<&str> = turn
        .messages
        .iter()
        .take(turn.messages.len().saturating_sub(1))
        .filter(|m| m.role != Role::System)
        .map(|m| m.content.as_str())
        .collect();
    result.supports = score::supports_in_prompt(&q.supports, &turn.packet.blocks, &history);

    // Retrieval before packing, to tell ranking misses from budget misses.
    let mut ropts = engine.retrieve_options().clone();
    if memory.is_some() {
        let scope = Scope::project(MEMORY_PROJECT).expect("valid scope");
        ropts.scopes = ropts.scopes.grant(scope);
    }
    match engine.retrieve(&q.question, &ropts) {
        Ok(found) => {
            let (first, all) = score::retrieval(&q.supports, &found.passages);
            result.first_rank = first;
            result.retrieved_all = all;
        }
        Err(e) => result.error = Some(e.to_string()),
    }
    result
}
