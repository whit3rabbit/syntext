//! Matching supports against prompts and retrievals, and aggregating.

use std::collections::BTreeMap;

use serde::Serialize;
use syntext_context::{EvidenceBlock, RankedPassage};

use super::{QuestionResult, Support};

/// Whether a support names `doc_key`: exactly, or as a `prefix:`.
fn names(support: &Support, doc_key: &str) -> bool {
    if support.doc.ends_with(':') {
        doc_key.starts_with(&support.doc)
    } else {
        doc_key == support.doc
    }
}

/// Per support, whether it reached the prompt: in an evidence block from
/// a matching document, or (for conversation turns) in the verbatim
/// history.
pub(super) fn supports_in_prompt(
    supports: &[Support],
    blocks: &[EvidenceBlock],
    history: &[&str],
) -> Vec<bool> {
    supports
        .iter()
        .map(|s| {
            let in_evidence = blocks
                .iter()
                .any(|b| names(s, &b.doc_key) && b.text.contains(&s.contains));
            let in_history =
                s.doc.starts_with("turn:") && history.iter().any(|h| h.contains(&s.contains));
            in_evidence || in_history
        })
        .collect()
}

/// Rank of the first supporting passage (1-based) and whether every
/// support was retrieved.
pub(super) fn retrieval(supports: &[Support], passages: &[RankedPassage]) -> (Option<usize>, bool) {
    let hit = |s: &Support, p: &RankedPassage| names(s, &p.doc_key) && p.text.contains(&s.contains);
    let first = passages
        .iter()
        .position(|p| supports.iter().any(|s| hit(s, p)))
        .map(|i| i + 1);
    let all = supports.iter().all(|s| passages.iter().any(|p| hit(s, p)));
    (first, all)
}

/// Aggregate results of one set at one window.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub set: String,
    pub window: usize,
    pub questions: usize,
    /// Questions whose every support reached the prompt.
    pub all_supported: usize,
    /// Supports that reached the prompt, over all supports.
    pub support_recall: f64,
    /// Questions whose every support was retrieved before packing.
    pub retrieved_all: usize,
    /// Mean reciprocal rank of the first supporting passage.
    pub mrr: f64,
    pub mean_evidence_tokens: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub errors: usize,
}

impl Summary {
    /// Share of questions fully supported.
    pub fn rate(&self) -> f64 {
        if self.questions == 0 {
            0.0
        } else {
            self.all_supported as f64 / self.questions as f64
        }
    }
}

/// Summaries grouped by set and window, in first-seen set order.
pub fn summarize(results: &[QuestionResult]) -> Vec<Summary> {
    let mut order: Vec<(String, usize)> = Vec::new();
    let mut groups: BTreeMap<(String, usize), Vec<&QuestionResult>> = BTreeMap::new();
    for r in results {
        let key = (r.set.clone(), r.window);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(r);
    }
    order
        .into_iter()
        .map(|key| {
            let rs = &groups[&key];
            let supports: usize = rs.iter().map(|r| r.supports.len()).sum();
            let found: usize = rs
                .iter()
                .map(|r| r.supports.iter().filter(|&&s| s).count())
                .sum();
            let mut ms: Vec<f64> = rs.iter().map(|r| r.micros as f64 / 1000.0).collect();
            ms.sort_by(f64::total_cmp);
            let pct = |p: f64| {
                if ms.is_empty() {
                    0.0
                } else {
                    ms[((ms.len() - 1) as f64 * p).round() as usize]
                }
            };
            let n = rs.len().max(1) as f64;
            Summary {
                set: key.0.clone(),
                window: key.1,
                questions: rs.len(),
                all_supported: rs.iter().filter(|r| r.all_supported()).count(),
                support_recall: if supports == 0 {
                    0.0
                } else {
                    found as f64 / supports as f64
                },
                retrieved_all: rs.iter().filter(|r| r.retrieved_all).count(),
                mrr: rs
                    .iter()
                    .map(|r| r.first_rank.map_or(0.0, |k| 1.0 / k as f64))
                    .sum::<f64>()
                    / n,
                mean_evidence_tokens: rs.iter().map(|r| r.evidence_tokens as f64).sum::<f64>() / n,
                p50_ms: pct(0.5),
                p95_ms: pct(0.95),
                errors: rs.iter().filter(|r| r.error.is_some()).count(),
            }
        })
        .collect()
}
