//! Evidence packing: ranked passages to a token-budgeted evidence packet.
//!
//! 1. Drop passages far below the best score (padding a small model's
//!    window with noise costs answer quality).
//! 2. Merge passages that sit next to each other in one document into one
//!    span, so adjacent hits arrive as continuous text, up to a share of
//!    the budget (beyond it, neighbours stay separate blocks).
//! 3. Cap spans per document and skip near-duplicates.
//! 4. Fill greedily by score; a span that does not fit whole is condensed
//!    (anchor plus focus windows) instead of dropped.
//! 5. With budget left over, widen accepted spans to neighbouring passages:
//!    an 8K window gets fuller context than a 4K one from the same ranking.
//! 6. Render, then verify the whole evidence text against the budget with
//!    the real counter, dropping the weakest block until it fits.

mod block;
mod budget;
mod condense;
mod expand;

use serde::{Deserialize, Serialize};

pub use budget::{Budget, MESSAGE_OVERHEAD};

use crate::analysis::term_set;
use crate::error::Result;
use crate::render::{render_evidence, RenderOptions};
use crate::retrieve::{Chain, RankedPassage, Signals};
use crate::tokens::TokenCounter;
use crate::types::{DocKind, PassageKind, Role, Trust};
pub(crate) use block::build_block;
use block::{document_order, jaccard};
use condense::{condense, Fit};

/// Order of blocks in the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrder {
    /// Best first.
    #[default]
    Ranked,
    /// Grouped by document (documents by best block), in line order.
    Document,
}

/// Packing limits.
#[derive(Debug, Clone)]
pub struct PackOptions {
    /// Most blocks in one packet.
    pub max_blocks: usize,
    /// Most blocks from one document.
    pub per_doc_cap: usize,
    /// Passages this many lines apart merge into one span.
    pub merge_gap_lines: u32,
    /// Largest share of the budget the passages merged into one span may
    /// add up to. Without it, a run of adjacent matches (say, a file of
    /// similar tests) becomes one block that crowds out everything else.
    pub max_span_share: f32,
    /// Context lines kept around each match when condensing.
    pub context_lines: u32,
    /// Passages scoring below this fraction of the best are dropped.
    pub relevance_floor: f32,
    /// Smallest budget remainder worth condensing a span into.
    pub min_block_tokens: usize,
    /// Widen spans into leftover budget.
    pub expand: bool,
    /// Term-set Jaccard similarity above which a span is a near-duplicate.
    pub near_duplicate: f32,
    /// Block order.
    pub order: EvidenceOrder,
}

impl Default for PackOptions {
    fn default() -> Self {
        PackOptions {
            max_blocks: 10,
            per_doc_cap: 3,
            merge_gap_lines: 3,
            max_span_share: 0.3,
            context_lines: 3,
            relevance_floor: 0.3,
            min_block_tokens: 48,
            expand: true,
            near_duplicate: 0.85,
            order: EvidenceOrder::Ranked,
        }
    }
}

/// One block of evidence as it appears in the prompt.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceBlock {
    /// Citation id (`E1`, `E2`, ...).
    pub id: String,
    /// Document key.
    pub doc_key: String,
    /// Path or title shown as the source.
    pub source: String,
    /// Location path.
    pub breadcrumb: String,
    /// Symbol defined by the best passage.
    pub symbol: Option<String>,
    /// 1-based first and last line covered.
    pub lines: (u32, u32),
    /// Byte range in the document text.
    pub byte_range: (usize, usize),
    /// Kind of the best passage.
    pub kind: PassageKind,
    /// Document kind.
    pub doc_kind: DocKind,
    /// Source trust.
    pub trust: Trust,
    /// Source scope.
    pub scope: String,
    /// Speaker, for turns.
    pub role: Option<Role>,
    /// Sanitized block text (possibly condensed).
    pub text: String,
    /// Score of the span: its best passage plus a bonus for each passage
    /// merged into it (at most 20% over the best passage).
    pub score: f32,
    /// Signals of the best passage.
    pub signals: Signals,
    /// Question terms matched.
    pub matched_terms: Vec<String>,
    /// Graph chain that reached the best passage.
    pub chain: Option<Chain>,
    /// Human-readable chain, rendered as the `via` attribute.
    pub via: Option<String>,
    /// Parts of the span were elided.
    pub condensed: bool,
    /// The span was widened beyond the retrieved passages.
    pub expanded: bool,
    /// Sanitization changed the text.
    pub sanitized: bool,
    /// Read from the live working tree.
    pub live: bool,
    /// Ids of the passages merged into this block.
    pub passage_ids: Vec<String>,
    /// Hex BLAKE3 digest of the source.
    pub source_hash: String,
    /// Rendered size in tokens.
    pub tokens: usize,
}

/// Why a candidate did not make it into the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// Score below the relevance floor.
    BelowFloor,
    /// Document already contributed `per_doc_cap` blocks.
    PerDocumentCap,
    /// Too similar to an included block.
    NearDuplicate,
    /// Did not fit the remaining budget, even condensed.
    Budget,
    /// `max_blocks` reached.
    MaxBlocks,
}

/// A candidate left out of the packet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Excluded {
    /// Document key.
    pub doc_key: String,
    /// Lines of the passage.
    pub lines: (u32, u32),
    /// Its score.
    pub score: f32,
    /// Why it was left out.
    pub reason: ExclusionReason,
}

pub(crate) struct Packed {
    pub blocks: Vec<EvidenceBlock>,
    pub excluded: Vec<Excluded>,
    pub evidence_text: String,
    pub tokens: usize,
}

/// Contiguous range of one document covering one or more passages.
pub(crate) struct Span<'a> {
    pub members: Vec<&'a RankedPassage>,
    pub first_idx: usize,
    pub last_idx: usize,
    pub start: usize,
    pub end: usize,
    pub lines: (u32, u32),
    pub score: f32,
    /// Tokens of the member passages.
    pub tokens: usize,
}

/// Cap on excluded entries recorded (the receipt is not a dump).
const MAX_EXCLUDED: usize = 16;
/// A merged span scores at most this multiple of its best passage: a run
/// of weak neighbours must not outrank a single strong passage.
const MERGE_BONUS_CAP: f32 = 1.2;

/// Pack `passages` into at most `budget` tokens of rendered evidence, with
/// citation ids numbered from `E<first_id>`.
pub(crate) fn pack(
    passages: &[RankedPassage],
    budget: usize,
    counter: &dyn TokenCounter,
    opts: &PackOptions,
    render: &RenderOptions,
    first_id: usize,
) -> Result<Packed> {
    let mut excluded = Vec::new();
    let mut exclude = |p: &RankedPassage, reason| {
        if excluded.len() < MAX_EXCLUDED {
            excluded.push(Excluded {
                doc_key: p.doc_key.clone(),
                lines: p.lines,
                score: p.score,
                reason,
            });
        }
    };
    let top = passages.first().map_or(0.0, |p| p.score);
    let floor = top * opts.relevance_floor;
    let span_cap = (budget as f32 * opts.max_span_share.clamp(0.0, 1.0)) as usize;
    let mut spans: Vec<Span<'_>> = Vec::new();
    for p in passages {
        // A lifted chain hop, or one of the nearest passages by vector, is
        // evidence of a kind the floor's single score scale cannot weigh:
        // it would drop the end of a chain, or a paraphrased answer,
        // whenever other passages echo the question's words more closely.
        if p.score < floor && !p.floor_exempt {
            exclude(p, ExclusionReason::BelowFloor);
            continue;
        }
        let gap = opts.merge_gap_lines;
        let tokens = counter.count(&p.text);
        let near = spans.iter_mut().find(|s| {
            let adjacent = p.index_in_doc + 1 >= s.first_idx && p.index_in_doc <= s.last_idx + 1;
            let close = p.lines.0 <= s.lines.1 + gap && s.lines.0 <= p.lines.1 + gap;
            s.members[0].access.same_doc(&p.access)
                && (adjacent || close)
                && s.tokens + tokens <= span_cap
        });
        match near {
            Some(s) => {
                s.members.push(p);
                s.first_idx = s.first_idx.min(p.index_in_doc);
                s.last_idx = s.last_idx.max(p.index_in_doc);
                s.start = s.start.min(p.byte_range.start);
                s.end = s.end.max(p.byte_range.end);
                s.lines = (s.lines.0.min(p.lines.0), s.lines.1.max(p.lines.1));
                s.tokens += tokens;
                s.score = (s.score + 0.15 * p.score).min(s.members[0].score * MERGE_BONUS_CAP);
            }
            None => spans.push(Span {
                members: vec![p],
                first_idx: p.index_in_doc,
                last_idx: p.index_in_doc,
                start: p.byte_range.start,
                end: p.byte_range.end,
                lines: p.lines,
                score: p.score,
                tokens,
            }),
        }
    }

    // A span holding several strong passages outranks a lone passage that
    // scored slightly higher; the stable sort keeps rank order on ties.
    spans.sort_by(|a, b| b.score.total_cmp(&a.score));

    let preamble_cost = counter.count(&render.preamble) + 2;
    let mut used = 0usize;
    let mut accepted: Vec<(Span<'_>, EvidenceBlock)> = Vec::new();
    let mut term_sets: Vec<Vec<String>> = Vec::new();
    for span in spans {
        let best = span.members[0];
        if accepted.len() >= opts.max_blocks {
            exclude(best, ExclusionReason::MaxBlocks);
            continue;
        }
        let per_doc = accepted
            .iter()
            .filter(|(s, _)| s.members[0].doc_key == best.doc_key)
            .count();
        if per_doc >= opts.per_doc_cap {
            exclude(best, ExclusionReason::PerDocumentCap);
            continue;
        }
        let terms = term_set(&best.text);
        if term_sets
            .iter()
            .any(|t| jaccard(t, &terms) > opts.near_duplicate)
        {
            exclude(best, ExclusionReason::NearDuplicate);
            continue;
        }
        let first_cost = if accepted.is_empty() {
            preamble_cost
        } else {
            2
        };
        let remaining = budget.saturating_sub(used + first_cost);
        let text = best.access.read(span.start, span.end)?;
        let block = build_block(&span, &text, false, false, counter, render);
        let block = if block.tokens <= remaining {
            Some(block)
        } else if remaining >= opts.min_block_tokens {
            let overhead = build_block(&span, "", false, false, counter, render).tokens;
            let anchor_line = best.lines.0 + best.anchor.0;
            let anchor = (anchor_line.saturating_sub(span.lines.0), best.anchor.1);
            let focus: Vec<u32> = span
                .members
                .iter()
                .flat_map(|m| m.focus_lines.iter().copied())
                .collect();
            let fit = Fit {
                counter,
                max_tokens: remaining,
                overhead,
            };
            condense(
                &text,
                span.lines.0,
                anchor,
                &focus,
                opts.context_lines,
                &fit,
            )
            .map(|c| build_block(&span, &c.text, true, false, counter, render))
            .filter(|b| b.tokens <= remaining)
        } else {
            None
        };
        match block {
            Some(b) => {
                used += first_cost + b.tokens;
                term_sets.push(terms);
                accepted.push((span, b));
            }
            None => exclude(best, ExclusionReason::Budget),
        }
    }

    if opts.expand && !accepted.is_empty() {
        expand::widen(&mut accepted, &mut used, budget, counter, render)?;
    }

    let mut blocks: Vec<EvidenceBlock> = match opts.order {
        EvidenceOrder::Ranked => accepted.into_iter().map(|(_, b)| b).collect(),
        EvidenceOrder::Document => document_order(accepted),
    };
    for (i, b) in blocks.iter_mut().enumerate() {
        b.id = format!("E{}", first_id + i);
    }
    let mut evidence_text = render_evidence(&blocks, render);
    let mut tokens = counter.count(&evidence_text);
    while tokens > budget && !blocks.is_empty() {
        let weakest = (0..blocks.len())
            .min_by(|&a, &b| blocks[a].score.total_cmp(&blocks[b].score))
            .unwrap_or(blocks.len() - 1);
        blocks.remove(weakest);
        for (i, b) in blocks.iter_mut().enumerate() {
            b.id = format!("E{}", first_id + i);
        }
        evidence_text = render_evidence(&blocks, render);
        tokens = counter.count(&evidence_text);
    }
    Ok(Packed {
        blocks,
        excluded,
        evidence_text,
        tokens,
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
