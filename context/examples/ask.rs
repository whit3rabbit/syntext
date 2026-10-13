//! Compile a directory into a context store and assemble a prompt for a
//! small-window model.
//!
//! ```sh
//! cargo run -p syntext-context --example ask -- <dir> "<question>" \
//!     [--window 8192] [--store <dir>] [--syntext] [--receipt]
//! ```
//!
//! Prints the chat messages that would be sent to an inference server
//! (llama.cpp, vLLM, any OpenAI-compatible endpoint) and a receipt summary.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use syntext_context::{
    ChatRequest, ContextEngine, ContextStore, DirectorySource, StoreOptions, SyntextSource,
};

struct Args {
    root: PathBuf,
    question: String,
    window: usize,
    store: PathBuf,
    syntext: bool,
    receipt: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut window = 8192;
    let mut store = None;
    let mut syntext = false;
    let mut receipt = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--window" => {
                let v = it.next().ok_or("--window needs a value")?;
                window = v.parse().map_err(|_| format!("bad --window {v}"))?;
            }
            "--store" => store = Some(PathBuf::from(it.next().ok_or("--store needs a value")?)),
            "--syntext" => syntext = true,
            "--receipt" => receipt = true,
            _ => positional.push(arg),
        }
    }
    let [root, question] = <[String; 2]>::try_from(positional).map_err(|_| {
        "usage: ask <dir> <question> [--window N] [--store DIR] [--syntext] [--receipt]"
    })?;
    let root = PathBuf::from(root);
    let store = store.unwrap_or_else(|| {
        let name: String = root
            .canonicalize()
            .unwrap_or_else(|_| root.clone())
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        std::env::temp_dir().join("syntext-context").join(name)
    });
    Ok(Args {
        root,
        question,
        window,
        store,
        syntext,
        receipt,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    let store = Arc::new(ContextStore::open_or_create(
        &args.store,
        StoreOptions::default(),
    )?);

    let t = Instant::now();
    let source = DirectorySource::new("repo", &args.root)?;
    let sync = store.sync_directory(&source)?;
    store.maybe_compact()?;
    eprintln!(
        "sync: {} scanned, +{} ~{} -{} in {:?}; store has {} documents / {} passages",
        sync.scanned,
        sync.added,
        sync.modified,
        sync.removed,
        t.elapsed(),
        store.stats().documents,
        store.stats().passages,
    );

    let mut builder = ContextEngine::builder(Arc::clone(&store));
    let mut _index = None;
    if args.syntext {
        let mut cfg = syntext::Config::new(args.store.join("syntext-index"), args.root.clone());
        cfg.auto_update = false;
        // Reuse the index from an earlier run; build it the first time.
        let index = match syntext::index::Index::open(cfg.clone()) {
            Ok(index) => index,
            Err(_) => syntext::index::Index::build(cfg)?,
        };
        let index = Arc::new(index);
        builder = builder.syntext(SyntextSource::new(Arc::clone(&index), "repo")?);
        _index = Some(index);
    }
    let engine = builder.build();

    let turn = engine.assemble(
        &ChatRequest::new(&args.question, args.window)
            .system("You are a careful assistant. Answer from the evidence and cite it."),
    )?;
    for m in &turn.messages {
        println!("===== {} =====\n{}\n", m.role.as_str(), m.content);
    }
    let r = &turn.packet.receipt;
    eprintln!(
        "window {}: prompt ~{} tokens, evidence {}/{} tokens in {} blocks, intent {:?}, {} candidates, {:.1} ms",
        args.window,
        turn.prompt_tokens,
        r.evidence_tokens,
        r.evidence_budget,
        r.evidence.len(),
        r.intent,
        r.candidates_considered,
        r.timings.total_us as f64 / 1000.0,
    );
    for e in &r.evidence {
        let graph = match &e.chain {
            Some(c) if !c.steps.is_empty() => format!(" via {}-hop chain", c.steps.len()),
            Some(c) => format!(" ({} {})", c.link, c.entity),
            None => String::new(),
        };
        eprintln!(
            "  [{}] {} lines {}-{} score {:.3}{}{}",
            e.id,
            e.source,
            e.lines.0,
            e.lines.1,
            e.score,
            if e.condensed { " condensed" } else { "" },
            graph,
        );
    }
    if args.receipt {
        println!("{}", r.to_json());
    }
    Ok(())
}
