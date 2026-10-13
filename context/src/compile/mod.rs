//! Compilation: source documents to passages, terms, and graph facts.
//!
//! Compilation is the offline half of the engine. It runs once per document
//! version (in parallel across documents) so that answering a question never
//! re-reads or re-segments the corpus.

pub(crate) mod directory;
mod source;
mod sync;

use std::collections::HashMap;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use syntext::changes::ContentDigest;

pub use directory::{DirectorySource, SyncReport};
pub use source::{decode_text, SourceDoc};
pub use sync::SyncProgress;

use crate::analysis::analyze_with;
use crate::graph::{self, GraphMode, PassageFacts};
use crate::structure::{segment, Lang, PassageSpan, SectionSpan, SegmentOptions};
use crate::tokens::{HeuristicCounter, TokenCounter};
use crate::types::{DocKey, DocKind, Role, Scope, Trust};

/// Version of the compiler's output: text analysis, segmentation, and graph
/// extraction. Bump it whenever a change would compile the same document
/// differently. Segments record the version that wrote them, and compaction
/// recompiles older ones, so a store never answers a query with one term
/// normalization against postings built with another.
pub const COMPILER_VERSION: u32 = 3;

/// Compile-time settings, persisted in the store manifest so every segment
/// of a store is built the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompileOptions {
    /// Passage sizing.
    pub segment: SegmentOptions,
    /// How much of the entity graph to extract.
    pub graph: GraphMode,
    /// How many times each term of a passage's breadcrumb and symbol name
    /// counts toward its term frequency (a BM25F-style field boost).
    pub title_weight: u32,
    /// Documents larger than this many bytes are skipped.
    pub max_doc_bytes: usize,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            segment: SegmentOptions::default(),
            graph: GraphMode::Full,
            title_weight: 3,
            max_doc_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Why a document was not compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Larger than [`CompileOptions::max_doc_bytes`].
    TooLarge,
    /// Contains NUL bytes (binary content).
    Binary,
}

/// Document-level metadata as stored.
#[derive(Debug, Clone)]
pub(crate) struct DocMeta {
    pub key: DocKey,
    pub kind: DocKind,
    pub lang: Lang,
    pub path: Option<String>,
    pub title: String,
    pub trust: Trust,
    pub scope: Scope,
    pub timestamp: i64,
    pub seq: u64,
    pub group: Option<String>,
    pub role: Option<Role>,
    pub source_hash: [u8; 32],
    pub text_hash: [u8; 32],
}

/// One compiled passage.
#[derive(Debug, Clone)]
pub(crate) struct CompiledPassage {
    pub span: PassageSpan,
    /// `(term, weighted frequency)`, sorted by term.
    pub terms: Vec<(String, u32)>,
    /// Content-term count, the BM25 document length.
    pub length: u32,
    /// Token estimate of the passage body.
    pub tokens: u32,
    pub facts: PassageFacts,
}

/// One compiled document, ready for the segment writer.
#[derive(Debug, Clone)]
pub(crate) struct CompiledDoc {
    pub meta: DocMeta,
    pub text: String,
    pub sections: Vec<SectionSpan>,
    pub passages: Vec<CompiledPassage>,
}

/// Compile documents in parallel. Returns compiled documents in input order
/// and the keys that were skipped.
pub(crate) fn compile_all(
    docs: Vec<SourceDoc>,
    opts: &CompileOptions,
) -> (Vec<CompiledDoc>, Vec<(DocKey, SkipReason)>) {
    let results: Vec<Result<CompiledDoc, (DocKey, SkipReason)>> =
        docs.into_par_iter().map(|d| compile_doc(d, opts)).collect();
    let mut compiled = Vec::with_capacity(results.len());
    let mut skipped = Vec::new();
    for r in results {
        match r {
            Ok(doc) => compiled.push(doc),
            Err(s) => skipped.push(s),
        }
    }
    (compiled, skipped)
}

/// Compile a single document.
pub(crate) fn compile_doc(
    doc: SourceDoc,
    opts: &CompileOptions,
) -> Result<CompiledDoc, (DocKey, SkipReason)> {
    if doc.text.len() > opts.max_doc_bytes {
        return Err((doc.key, SkipReason::TooLarge));
    }
    if doc.text.chars().take(8192).any(|c| c == '\0') {
        return Err((doc.key, SkipReason::Binary));
    }
    let lang = doc
        .lang
        .unwrap_or_else(|| Lang::detect(doc.path.as_deref(), &doc.text));
    let location = doc.location();
    let layout = segment(&doc.text, lang, doc.kind, &location, &opts.segment);
    let counter = HeuristicCounter::new();
    let passages = layout
        .passages
        .into_iter()
        .map(|span| {
            let body = &doc.text[span.start..span.end];
            let title = match &span.symbol {
                Some(sym) => format!("{} {}", span.breadcrumb, sym.name),
                None => span.breadcrumb.clone(),
            };
            let (terms, length) = passage_terms(body, &title, opts.title_weight);
            let facts = graph::extract(&doc.text, &span, lang, opts.graph);
            CompiledPassage {
                tokens: counter.count(body).min(u32::MAX as usize) as u32,
                span,
                terms,
                length,
                facts,
            }
        })
        .collect();
    let text_hash = *ContentDigest::from_bytes(doc.text.as_bytes()).as_bytes();
    let source_hash = doc.source_hash.map_or(text_hash, |d| *d.as_bytes());
    let title = doc
        .title
        .clone()
        .unwrap_or_else(|| default_title(&doc, &location));
    Ok(CompiledDoc {
        meta: DocMeta {
            key: doc.key,
            kind: doc.kind,
            lang,
            path: doc.path,
            title,
            trust: doc.trust,
            scope: doc.scope,
            timestamp: doc.timestamp.unwrap_or(0),
            seq: doc.seq,
            group: doc.group,
            role: doc.role,
            source_hash,
            text_hash,
        },
        text: doc.text,
        sections: layout.sections,
        passages,
    })
}

fn default_title(doc: &SourceDoc, location: &str) -> String {
    match &doc.path {
        Some(p) => p.rsplit('/').next().unwrap_or(p).to_string(),
        None => location.to_string(),
    }
}

/// Weighted term frequencies for a passage and its content-term count.
fn passage_terms(body: &str, title: &str, title_weight: u32) -> (Vec<(String, u32)>, u32) {
    let mut tf: HashMap<String, u32> = HashMap::new();
    let mut length = 0u32;
    analyze_with(body, |t| {
        if !t.compound {
            length = length.saturating_add(1);
        }
        let e = tf.entry(t.text).or_insert(0);
        *e = e.saturating_add(1);
    });
    if title_weight > 0 {
        analyze_with(title, |t| {
            let e = tf.entry(t.text).or_insert(0);
            *e = e.saturating_add(title_weight);
        });
    }
    let mut terms: Vec<(String, u32)> = tf.into_iter().collect();
    terms.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    (terms, length)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
