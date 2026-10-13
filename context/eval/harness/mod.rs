//! Evaluation harness for the context engine.
//!
//! A set is a corpus plus questions whose supporting evidence is known. Each
//! question runs end to end through `ContextEngine::assemble` at every
//! window, and is scored by whether every supporting passage reached the
//! prompt the model would see: the measure that matters for a small window,
//! since the model can only use what it was given.
//!
//! Shared by `examples/eval.rs` (reports) and `tests/eval.rs` (regression
//! gate) through `#[path]`, so none of it is part of the library API.
#![allow(dead_code)]

mod corpus;
pub mod hotpot;
pub mod report;
mod score;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use score::{summarize, Summary};

/// A corpus and the questions asked of it.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalSet {
    /// Short name used in reports.
    pub name: String,
    /// What the set measures.
    #[serde(default)]
    pub description: String,
    /// What the store is built from.
    pub corpus: Corpus,
    /// Questions with known supporting evidence.
    pub questions: Vec<Question>,
}

/// Where a set's documents come from.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Corpus {
    /// Inline notes, keyed `note:<id>`.
    Notes { docs: Vec<Note> },
    /// This repository (the parent of the `context/` crate), keyed
    /// `<label>:<path>`.
    Repository {
        label: String,
        #[serde(default)]
        exclude: Vec<String>,
        /// Attach a syntext index for exact identifier matches.
        #[serde(default)]
        syntext: bool,
    },
    /// A directory supplied at run time with `--corpus <name>=<dir>`; the
    /// set is skipped without one.
    External {
        name: String,
        label: String,
        #[serde(default)]
        exclude: Vec<String>,
        #[serde(default)]
        syntext: bool,
    },
    /// A conversation recorded into memory before the questions are asked,
    /// plus optional notes.
    Conversation {
        turns: Vec<TurnSpec>,
        #[serde(default)]
        docs: Vec<Note>,
    },
}

/// A note in a `notes` or `conversation` corpus.
#[derive(Debug, Clone, Deserialize)]
pub struct Note {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub text: String,
}

/// One recorded conversation turn.
#[derive(Debug, Clone, Deserialize)]
pub struct TurnSpec {
    /// `user` or `assistant`.
    pub role: String,
    pub content: String,
}

/// A question and the evidence an answer needs.
#[derive(Debug, Clone, Deserialize)]
pub struct Question {
    pub id: String,
    pub question: String,
    /// Every passage the answer depends on.
    pub supports: Vec<Support>,
    /// Free-form labels for slicing results (`2-hop`, `no-overlap`, ...).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Evidence an answer needs: text from a document that must reach the
/// prompt.
#[derive(Debug, Clone, Deserialize)]
pub struct Support {
    /// Document key (`note:refunds`, `repo:src/lib.rs`). A key ending in
    /// `:` matches every document with that prefix (`turn:` matches any
    /// conversation turn).
    pub doc: String,
    /// Text that must appear in the evidence taken from that document.
    pub contains: String,
}

/// Settings for one run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Model windows to assemble prompts for.
    pub windows: Vec<usize>,
    /// Directories for `external` corpora, by name.
    pub external: BTreeMap<String, PathBuf>,
    /// Root of this repository, for `repository` corpora.
    pub repo_root: PathBuf,
    /// System prompt used for every question.
    pub system: String,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            windows: vec![4096, 8192],
            external: BTreeMap::new(),
            repo_root: repo_root(),
            system: "You are a careful assistant. Answer from the evidence.".to_string(),
        }
    }
}

/// The repository root: the parent of the `context/` crate.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("context/ has a parent")
        .to_path_buf()
}

/// The built-in sets, from `context/eval/sets/`.
pub fn builtin_sets() -> Result<Vec<EvalSet>, String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/sets");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths.iter().map(|p| load_set(p)).collect()
}

/// Parse one set file.
pub fn load_set(path: &Path) -> Result<EvalSet, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Result of one question at one window.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionResult {
    pub set: String,
    pub id: String,
    pub window: usize,
    pub tags: Vec<String>,
    /// Per support: whether it reached the prompt.
    pub supports: Vec<bool>,
    /// 1-based rank of the first supporting passage in retrieval, before
    /// packing.
    pub first_rank: Option<usize>,
    /// Whether every support was among the retrieved passages (before the
    /// budget was applied).
    pub retrieved_all: bool,
    pub evidence_tokens: usize,
    pub blocks: usize,
    pub prompt_tokens: usize,
    /// Wall-clock time of the `assemble` call.
    pub micros: u64,
    pub error: Option<String>,
}

impl QuestionResult {
    /// Every support reached the prompt.
    pub fn all_supported(&self) -> bool {
        self.error.is_none() && self.supports.iter().all(|&s| s)
    }
}

/// What happened to one set.
#[derive(Debug)]
pub enum SetOutcome {
    /// Results for every question at every window.
    Ran(Vec<QuestionResult>),
    /// Not run, and why (for example an external corpus was not supplied).
    Skipped(String),
}

/// Build a store for `set`, then ask every question at every window.
pub fn run_set(set: &EvalSet, opts: &RunOptions) -> Result<SetOutcome, String> {
    corpus::run(set, opts)
}
