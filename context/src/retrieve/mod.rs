//! Retrieval: a question to ranked, scored passages.
//!
//! ```text
//! question --analyze--> terms, identifiers, phrases, entity keys, intent
//!   |-- lexical: BM25 over postings, rarest terms first, budgeted
//!   |-- graph:   entity seeds -> bounded multi-hop walk -> evidence chains
//!   '-- exact:   syntext identifier search -> stored or live passages
//! pool --rerank--> coverage, proximity, phrase, title, symbol, structure,
//!                  recency, graph, exact -> weighted score -> top passages
//! ```

pub(crate) mod access;
mod candidates;
mod chains;
mod graph_walk;
mod pipeline;
mod query;
mod rerank;
mod syntext_source;

use std::ops::Range;

use serde::{Deserialize, Serialize};

pub(crate) use access::DocAccess;
pub use graph_walk::{Chain, ChainStep};
pub(crate) use pipeline::retrieve;
pub use query::QueryIntent;
pub use syntext_source::SyntextSource;

use crate::types::{DocKey, DocKind, PassageKind, Role, ScopeFilter, Trust};

/// Weights of the ranking signals (each signal is normalized to 0-1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RankWeights {
    /// BM25F relevance, normalized by the best candidate.
    pub bm25: f32,
    /// IDF-weighted share of the question's terms present.
    pub coverage: f32,
    /// How tightly the matched terms cluster.
    pub proximity: f32,
    /// Exact phrases and adjacent term pairs.
    pub phrase: f32,
    /// Question terms in the breadcrumb or symbol name.
    pub title: f32,
    /// The passage defines a symbol the question names.
    pub symbol: f32,
    /// A syntext index verified an identifier match in the live file.
    pub exact: f32,
    /// Entity-graph activation.
    pub graph: f32,
    /// Intent-aware prior on the passage kind.
    pub structure: f32,
    /// Newer conversation turns (tripled for recall questions).
    pub recency: f32,
}

impl Default for RankWeights {
    fn default() -> Self {
        RankWeights {
            bm25: 0.30,
            coverage: 0.25,
            proximity: 0.10,
            phrase: 0.10,
            title: 0.06,
            symbol: 0.12,
            exact: 0.12,
            graph: 0.18,
            structure: 0.06,
            recency: 0.04,
        }
    }
}

/// Retrieval limits and filters.
#[derive(Debug, Clone)]
pub struct RetrieveOptions {
    /// Passages returned.
    pub max_results: usize,
    /// First-phase lexical candidates kept.
    pub candidate_limit: usize,
    /// Candidates reranked (text is read only for these).
    pub rerank_limit: usize,
    /// Postings traversed before common terms are left to the reranker.
    pub postings_budget: usize,
    /// Entity-graph hops (0 disables the graph).
    pub hops: u8,
    /// Entities kept per hop.
    pub beam: usize,
    /// Signal weights.
    pub weights: RankWeights,
    /// Scopes the request may read. Defaults to the default scope only.
    pub scopes: ScopeFilter,
    /// Documents to leave out (e.g. turns already in the prompt).
    pub exclude: Vec<DocKey>,
    /// Override the detected intent.
    pub intent: Option<QueryIntent>,
}

impl Default for RetrieveOptions {
    fn default() -> Self {
        RetrieveOptions {
            max_results: 24,
            candidate_limit: 400,
            rerank_limit: 200,
            postings_budget: 200_000,
            hops: 2,
            beam: 16,
            weights: RankWeights::default(),
            scopes: ScopeFilter::default(),
            exclude: Vec::new(),
            intent: None,
        }
    }
}

/// Per-signal values behind a passage's score (each 0-1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    /// BM25F relevance relative to the best candidate.
    pub bm25: f32,
    /// IDF-weighted share of question terms present.
    pub coverage: f32,
    /// Clustering of matched terms.
    pub proximity: f32,
    /// Exact phrases and adjacent pairs.
    pub phrase: f32,
    /// Question terms in the breadcrumb or symbol.
    pub title: f32,
    /// Defines a symbol the question names.
    pub symbol: f32,
    /// Verified identifier match in the live file.
    pub exact: f32,
    /// Entity-graph activation.
    pub graph: f32,
    /// Intent-aware kind prior.
    pub structure: f32,
    /// Conversation recency.
    pub recency: f32,
}

/// Wall-clock time spent per stage, in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageTimings {
    /// Question analysis and visibility setup.
    pub analyze_us: u64,
    /// Lexical candidate generation.
    pub lexical_us: u64,
    /// Entity-graph walk.
    pub graph_us: u64,
    /// Exact search through syntext.
    pub exact_us: u64,
    /// Reranking.
    pub rerank_us: u64,
    /// Evidence packing (set by `prepare`).
    pub pack_us: u64,
    /// End to end.
    pub total_us: u64,
}

/// How the question was understood.
#[derive(Debug, Clone, Serialize)]
pub struct QueryReport {
    /// Intent used for structural priors.
    pub intent: QueryIntent,
    /// Normalized terms.
    pub terms: Vec<String>,
    /// Identifiers searched exactly.
    pub identifiers: Vec<String>,
    /// Graph entities the question named (seeds).
    pub entities: Vec<String>,
}

/// One retrieved passage with its provenance and score breakdown.
#[derive(Clone)]
pub struct RankedPassage {
    /// Document key.
    pub doc_key: String,
    /// Document title.
    pub title: String,
    /// Repository-relative path, for files.
    pub path: Option<String>,
    /// Location path (file > section/container).
    pub breadcrumb: String,
    /// Symbol the passage defines, if any.
    pub symbol: Option<String>,
    /// Passage kind.
    pub kind: PassageKind,
    /// Document kind.
    pub doc_kind: DocKind,
    /// Trust of the source.
    pub trust: Trust,
    /// Scope of the source.
    pub scope: String,
    /// Speaker, for conversation turns.
    pub role: Option<Role>,
    /// Byte range in the document's text.
    pub byte_range: Range<usize>,
    /// 1-based first and last line.
    pub lines: (u32, u32),
    /// Passage text.
    pub text: String,
    /// Final score.
    pub score: f32,
    /// Signal breakdown.
    pub signals: Signals,
    /// Question terms found in the passage.
    pub matched_terms: Vec<String>,
    /// Absolute line numbers holding matches.
    pub focus_lines: Vec<u32>,
    /// Graph path that reached this passage, if any.
    pub chain: Option<Chain>,
    /// Hex BLAKE3 digest of the source bytes the passage came from.
    pub source_hash: String,
    /// Stable identifier of this passage version.
    pub passage_id: String,
    /// True when read from the live working tree because the store was
    /// stale or did not hold the file.
    pub live: bool,
    pub(crate) access: DocAccess,
    pub(crate) index_in_doc: usize,
    pub(crate) anchor: (u32, u32),
    pub(crate) seq: u64,
}

impl std::fmt::Debug for RankedPassage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RankedPassage")
            .field("doc_key", &self.doc_key)
            .field("lines", &self.lines)
            .field("score", &self.score)
            .finish()
    }
}

/// Result of a retrieval.
#[derive(Debug, Clone)]
pub struct Retrieved {
    /// Store generation searched.
    pub generation: u64,
    /// Store identifier.
    pub store_id: String,
    /// How the question was understood.
    pub query: QueryReport,
    /// Passages, best first.
    pub passages: Vec<RankedPassage>,
    /// Candidates scored before truncation.
    pub candidates_considered: usize,
    /// Stage timings.
    pub timings: StageTimings,
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
