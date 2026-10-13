//! Budget-aware widening: spend leftover budget around accepted spans.
//!
//! Retrieval picks the passages that match; the model often also needs the
//! rest of the section or the code right after a signature. When the
//! evidence leaves budget unused, each accepted (uncondensed) span first
//! tries to become its whole enclosing section, then grows by one passage on
//! either side, best spans first, never into another accepted span of the
//! same document.

use super::{build_block, EvidenceBlock, Span};
use crate::error::Result;
use crate::render::RenderOptions;
use crate::retrieve::access::PassageInfo;
use crate::retrieve::RankedPassage;
use crate::tokens::TokenCounter;

/// Passes over the accepted spans.
const PASSES: usize = 2;

pub(super) fn widen(
    accepted: &mut [(Span<'_>, EvidenceBlock)],
    used: &mut usize,
    budget: usize,
    counter: &dyn TokenCounter,
    render: &RenderOptions,
) -> Result<()> {
    for _ in 0..PASSES {
        let mut grew = false;
        for i in 0..accepted.len() {
            let remaining = budget.saturating_sub(*used);
            if remaining < budget / 8 {
                return Ok(());
            }
            let (span, block) = &accepted[i];
            if block.condensed {
                continue;
            }
            let best = span.members[0];
            let passages = best.access.passages();
            let Some(last) = passages.len().checked_sub(1) else {
                continue;
            };
            let lo = span.first_idx.saturating_sub(1);
            let hi = (span.last_idx + 1).min(last);
            let mut options = Vec::with_capacity(4);
            if let Some(range) = section_range(&passages, best, span) {
                options.push(range);
            }
            options.extend([(lo, hi), (span.first_idx, hi), (lo, span.last_idx)]);
            for (a, b) in options {
                if (a, b) == (span.first_idx, span.last_idx) || b < a {
                    continue;
                }
                let (start, end) = (
                    passages[a].start.min(span.start),
                    passages[b].end.max(span.end),
                );
                let collides = accepted.iter().enumerate().any(|(j, (other, _))| {
                    j != i
                        && other.members[0].access.same_doc(&best.access)
                        && start < other.end
                        && other.start < end
                });
                if collides {
                    continue;
                }
                let text = best.access.read(start, end)?;
                let grown = Span {
                    members: span.members.clone(),
                    first_idx: a.min(span.first_idx),
                    last_idx: b.max(span.last_idx),
                    start,
                    end,
                    lines: (
                        passages[a].line_start.min(span.lines.0),
                        passages[b].line_end.max(span.lines.1),
                    ),
                    score: span.score,
                    tokens: span.tokens,
                };
                let new_block = build_block(&grown, &text, false, true, counter, render);
                if new_block.tokens <= block.tokens + remaining {
                    *used = *used + new_block.tokens - block.tokens.min(new_block.tokens);
                    accepted[i] = (grown, new_block);
                    grew = true;
                    break;
                }
            }
        }
        if !grew {
            break;
        }
    }
    Ok(())
}

/// Passage index range of the section enclosing the best passage, when it
/// is larger than the span.
fn section_range(
    passages: &[PassageInfo],
    best: &RankedPassage,
    span: &Span<'_>,
) -> Option<(usize, usize)> {
    let section = passages.get(best.index_in_doc)?.section?;
    let (start, end) = best.access.section(section)?;
    let a = passages
        .iter()
        .position(|p| p.start >= start && p.end <= end)?;
    let b = passages
        .iter()
        .rposition(|p| p.start >= start && p.end <= end)?;
    let covers = a <= span.first_idx && b >= span.last_idx;
    (covers && (a, b) != (span.first_idx, span.last_idx)).then_some((a, b))
}
