//! Answer receipts: exactly what evidence a model was given, and why.
//!
//! A receipt records the packet the model actually saw, not the candidate
//! list: each block's source, byte and line range, BLAKE3 digest of the
//! source version, merged passage ids, score breakdown, graph chain, and
//! whether it was condensed, widened, sanitized, or read live. Together
//! with the store generation this answers "which revision of which file
//! supported this claim?" long after the fact, and [`Receipt::check_sources`]
//! tells whether those sources have changed since.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compile::directory::file_digest;
use crate::error::{ContextError, Result};
use crate::pack::{EvidenceBlock, Excluded};
use crate::retrieve::{Chain, QueryIntent, Signals, StageTimings};
use crate::types::{PassageKind, Trust};

/// Receipt format version.
pub const RECEIPT_VERSION: u32 = 1;

/// Provenance of one evidence block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Citation id in the prompt.
    pub id: String,
    /// Document key.
    pub doc_key: String,
    /// Path or title.
    pub source: String,
    /// Hex BLAKE3 digest of the source bytes the evidence came from.
    pub source_hash: String,
    /// Retrieved passages merged into the block.
    pub passage_ids: Vec<String>,
    /// Byte range in the document text.
    pub byte_range: (usize, usize),
    /// 1-based line range.
    pub lines: (u32, u32),
    /// Passage kind.
    pub kind: PassageKind,
    /// Source trust.
    pub trust: Trust,
    /// Source scope.
    pub scope: String,
    /// Block score.
    pub score: f32,
    /// Score breakdown of the best passage.
    pub signals: Signals,
    /// Question terms matched.
    pub matched_terms: Vec<String>,
    /// Entity-graph chain, when the graph contributed.
    pub chain: Option<Chain>,
    /// Text was elided to fit.
    pub condensed: bool,
    /// Span was widened with neighbouring passages.
    pub expanded: bool,
    /// Sanitization altered the text.
    pub sanitized: bool,
    /// Read from the live working tree.
    pub live: bool,
    /// Rendered tokens.
    pub tokens: usize,
}

impl From<&EvidenceBlock> for EvidenceRef {
    fn from(b: &EvidenceBlock) -> Self {
        EvidenceRef {
            id: b.id.clone(),
            doc_key: b.doc_key.clone(),
            source: b.source.clone(),
            source_hash: b.source_hash.clone(),
            passage_ids: b.passage_ids.clone(),
            byte_range: b.byte_range,
            lines: b.lines,
            kind: b.kind,
            trust: b.trust,
            scope: b.scope.clone(),
            score: b.score,
            signals: b.signals,
            matched_terms: b.matched_terms.clone(),
            chain: b.chain.clone(),
            condensed: b.condensed,
            expanded: b.expanded,
            sanitized: b.sanitized,
            live: b.live,
            tokens: b.tokens,
        }
    }
}

/// Record of one evidence packet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    /// [`RECEIPT_VERSION`].
    pub version: u32,
    /// Unix seconds.
    pub created_unix: i64,
    /// Store identifier.
    pub store_id: String,
    /// Store generation searched.
    pub generation: u64,
    /// The question.
    pub question: String,
    /// Intent used.
    pub intent: QueryIntent,
    /// Normalized question terms.
    pub terms: Vec<String>,
    /// Identifiers searched exactly.
    pub identifiers: Vec<String>,
    /// Graph entities the question named.
    pub entities: Vec<String>,
    /// Evidence given to the model, in prompt order.
    pub evidence: Vec<EvidenceRef>,
    /// Strong candidates left out, and why.
    pub excluded: Vec<Excluded>,
    /// Tokens allowed for evidence.
    pub evidence_budget: usize,
    /// Tokens used by the rendered evidence.
    pub evidence_tokens: usize,
    /// Candidates scored.
    pub candidates_considered: usize,
    /// Stage timings.
    pub timings: StageTimings,
    /// Token counter used.
    pub tokenizer: String,
    /// Model the packet was prepared for.
    pub model: Option<String>,
    /// Embedder whose vectors ranked the evidence, if any.
    #[serde(default)]
    pub embedder: Option<String>,
}

/// Whether a cited source still matches what the model saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// Same bytes as when the evidence was taken.
    Unchanged,
    /// The file changed since.
    Changed,
    /// The file no longer exists.
    Missing,
    /// Not a file under the checked label (a note, turn, or other source).
    NotChecked,
}

/// Result of checking one evidence block's source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCheck {
    /// Evidence id.
    pub id: String,
    /// Document key.
    pub doc_key: String,
    /// Status.
    pub status: SourceStatus,
}

impl Receipt {
    /// Pretty-printed JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Parse a receipt from JSON.
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).map_err(|e| ContextError::invalid(format!("receipt: {e}")))
    }

    /// Re-hash the cited files under `root` (documents keyed
    /// `<label>:<path>`) and report which changed since the evidence was
    /// taken.
    pub fn check_sources(&self, root: &Path, label: &str) -> Vec<SourceCheck> {
        let prefix = format!("{label}:");
        self.evidence
            .iter()
            .map(|e| {
                let status = match e.doc_key.strip_prefix(&prefix) {
                    None => SourceStatus::NotChecked,
                    Some(rel) if !is_plain_relative(rel) => SourceStatus::NotChecked,
                    Some(rel) => match file_digest(&root.join(rel)) {
                        None => SourceStatus::Missing,
                        Some(d) if d.to_hex() == e.source_hash => SourceStatus::Unchanged,
                        Some(_) => SourceStatus::Changed,
                    },
                };
                SourceCheck {
                    id: e.id.clone(),
                    doc_key: e.doc_key.clone(),
                    status,
                }
            })
            .collect()
    }
}

/// A receipt read back from disk is untrusted: never let a doc key steer a
/// path outside `root`.
fn is_plain_relative(rel: &str) -> bool {
    let p = Path::new(rel);
    !rel.is_empty()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
