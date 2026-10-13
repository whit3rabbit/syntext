//! Building evidence blocks from spans, and small packing helpers.

use super::{EvidenceBlock, Span};
use crate::render::sanitize::sanitize;
use crate::render::{render_block, RenderOptions};
use crate::retrieve::Chain;
use crate::tokens::TokenCounter;

pub(super) fn document_order(accepted: Vec<(Span<'_>, EvidenceBlock)>) -> Vec<EvidenceBlock> {
    let mut docs: Vec<String> = Vec::new();
    for (_, b) in &accepted {
        if !docs.contains(&b.doc_key) {
            docs.push(b.doc_key.clone());
        }
    }
    let mut blocks: Vec<EvidenceBlock> = accepted.into_iter().map(|(_, b)| b).collect();
    blocks.sort_by_key(|b| (docs.iter().position(|d| d == &b.doc_key), b.lines.0));
    blocks
}

/// Build a block for `span` with `body` as its text.
pub(crate) fn build_block(
    span: &Span<'_>,
    body: &str,
    condensed: bool,
    expanded: bool,
    counter: &dyn TokenCounter,
    render: &RenderOptions,
) -> EvidenceBlock {
    let best = span.members[0];
    let (text, sanitized) = sanitize(body, &render.tag, &render.sanitize);
    let mut block = EvidenceBlock {
        id: "E00".to_string(),
        doc_key: best.doc_key.clone(),
        source: best.path.clone().unwrap_or_else(|| best.title.clone()),
        breadcrumb: best.breadcrumb.clone(),
        symbol: best.symbol.clone(),
        lines: span.lines,
        byte_range: (span.start, span.end),
        kind: best.kind,
        doc_kind: best.doc_kind,
        trust: best.trust,
        scope: best.scope.clone(),
        role: best.role,
        text,
        score: span.score,
        signals: best.signals,
        matched_terms: best.matched_terms.clone(),
        chain: best.chain.clone(),
        via: best.chain.as_ref().and_then(describe_chain),
        condensed,
        expanded,
        sanitized,
        live: best.live,
        passage_ids: span.members.iter().map(|m| m.passage_id.clone()).collect(),
        source_hash: best.source_hash.clone(),
        tokens: 0,
    };
    block.tokens = counter.count(&render_block(&block, render));
    block
}

/// `A -[works for]-> B; B -[acquired]-> C`, or `None` without steps.
fn describe_chain(chain: &Chain) -> Option<String> {
    if chain.steps.is_empty() {
        return None;
    }
    let steps: Vec<String> = chain
        .steps
        .iter()
        .map(|s| format!("{} -[{}]-> {}", s.from, s.relation, s.to))
        .collect();
    Some(steps.join("; "))
}

pub(super) fn jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.iter().filter(|t| b.binary_search(t).is_ok()).count();
    inter as f32 / (a.len() + b.len() - inter) as f32
}
