//! Size normalization: split oversized passages, merge undersized ones.
//!
//! Structural segmentation decides *where* passages may begin; this pass
//! keeps them inside the size budget. Splits prefer blank lines, then line
//! boundaries; a single line longer than the budget (minified code, data
//! blobs) is cut at character boundaries as a last resort. Merges only join
//! neighbours from the same section or container, so a merged passage never
//! straddles a structural boundary.

use super::lines::Lines;
use super::{Layout, PassageSpan, SegmentOptions};
use crate::types::{PassageKind, SymbolKind};

/// Cap on passages per document; beyond it the remainder is dropped (with a
/// warning) rather than letting one pathological file dominate a segment.
const MAX_PASSAGES_PER_DOC: usize = 50_000;

pub(super) fn normalize(layout: &mut Layout, lines: &Lines<'_>, text: &str, opts: &SegmentOptions) {
    let mut out = Vec::with_capacity(layout.passages.len());
    for p in layout.passages.drain(..) {
        if text[p.start..p.end].trim().is_empty() {
            continue;
        }
        if p.len() > opts.max_passage_bytes {
            split_oversized(p, lines, text, opts, &mut out);
        } else {
            out.push(p);
        }
    }
    merge_small(&mut out, opts);
    if out.len() > MAX_PASSAGES_PER_DOC {
        log::warn!(
            "document produced {} passages; keeping the first {MAX_PASSAGES_PER_DOC}",
            out.len()
        );
        out.truncate(MAX_PASSAGES_PER_DOC);
    }
    layout.passages = out;
}

fn split_oversized(
    p: PassageSpan,
    lines: &Lines<'_>,
    text: &str,
    opts: &SegmentOptions,
    out: &mut Vec<PassageSpan>,
) {
    let max = opts.max_passage_bytes;
    let first_line = lines.line_of(p.start);
    let last_line = lines.line_of(p.end.saturating_sub(1));
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    let mut chunk_start = p.start;
    let mut last_blank: Option<usize> = None;
    for l in first_line..=last_line {
        let ls = lines.start(l).max(p.start);
        let le = lines.start(l + 1).min(p.end);
        if le - ls > max {
            if ls > chunk_start {
                chunks.push((chunk_start, ls));
            }
            hard_split(text, ls, le, max, &mut chunks);
            chunk_start = le;
            last_blank = None;
            continue;
        }
        if le - chunk_start > max && ls > chunk_start {
            // Prefer the last blank line in the back half of the chunk.
            let cut = match last_blank {
                Some(b) if b - chunk_start >= max / 2 => b,
                _ => ls,
            };
            chunks.push((chunk_start, cut));
            chunk_start = cut;
            last_blank = None;
        }
        if text[ls..le].trim().is_empty() && ls > chunk_start {
            last_blank = Some(ls);
        }
    }
    if p.end > chunk_start {
        chunks.push((chunk_start, p.end));
    }
    for (s, e) in chunks {
        if text[s..e].trim().is_empty() {
            continue;
        }
        let mut c = p.clone();
        c.start = s;
        c.end = e;
        c.line_start = lines.line_of(s) as u32 + 1;
        c.line_end = lines.line_of(e.saturating_sub(1)).max(lines.line_of(s)) as u32 + 1;
        // The chunk holding the anchor (heading or signature) keeps it and
        // the symbol; the others hold body text only.
        let anchor_line = p.line_start + p.anchor_offset;
        if p.anchor_len > 0 && (c.line_start..=c.line_end).contains(&anchor_line) {
            c.anchor_offset = anchor_line - c.line_start;
        } else {
            c.anchor_offset = 0;
            c.anchor_len = 0;
            if c.kind == PassageKind::Definition {
                c.kind = PassageKind::Code;
                c.symbol = None;
            }
        }
        out.push(c);
    }
}

/// Cut `[s, e)` into pieces of at most `max` bytes at char boundaries.
fn hard_split(text: &str, s: usize, e: usize, max: usize, out: &mut Vec<(usize, usize)>) {
    let mut a = s;
    while a < e {
        let mut b = (a + max).min(e);
        while b < e && !text.is_char_boundary(b) {
            b -= 1;
        }
        if b == a {
            // `max` is smaller than one character: take the whole character.
            b = a + 1;
            while b < e && !text.is_char_boundary(b) {
                b += 1;
            }
        }
        out.push((a, b));
        a = b;
    }
}

fn merge_small(ps: &mut Vec<PassageSpan>, opts: &SegmentOptions) {
    let mut i = 0;
    while i < ps.len() {
        if ps[i].len() >= opts.min_passage_bytes {
            i += 1;
            continue;
        }
        if i + 1 < ps.len() && can_merge(&ps[i], &ps[i + 1], opts) {
            let next = ps.remove(i + 1);
            absorb(&mut ps[i], next);
            continue;
        }
        if i > 0 && can_merge(&ps[i - 1], &ps[i], opts) {
            let cur = ps.remove(i);
            absorb(&mut ps[i - 1], cur);
            continue;
        }
        i += 1;
    }
}

fn can_merge(a: &PassageSpan, b: &PassageSpan, opts: &SegmentOptions) -> bool {
    a.section == b.section
        && a.breadcrumb == b.breadcrumb
        && class(a.kind) == class(b.kind)
        && b.end - a.start <= opts.max_passage_bytes
}

/// Kinds that may share a passage.
fn class(kind: PassageKind) -> u8 {
    match kind {
        PassageKind::Prose => 0,
        PassageKind::Code | PassageKind::Definition => 1,
        PassageKind::Turn => 2,
        PassageKind::Data => 3,
    }
}

/// Merge `b` (which directly follows `a`) into `a`.
fn absorb(a: &mut PassageSpan, b: PassageSpan) {
    if a.anchor_len == 0 && b.anchor_len > 0 {
        a.anchor_offset = b.line_start - a.line_start + b.anchor_offset;
        a.anchor_len = b.anchor_len;
    }
    let prefer_b = match (&a.symbol, &b.symbol) {
        (None, Some(_)) => true,
        (Some(sa), Some(sb)) => rank(sb.kind) > rank(sa.kind),
        _ => false,
    };
    if prefer_b {
        a.symbol = b.symbol;
    }
    // `can_merge` only joins kinds of one class, so they differ only for
    // code, where a named symbol makes the merged passage a definition.
    if class(a.kind) == 1 {
        a.kind = if a.symbol.is_some() {
            PassageKind::Definition
        } else {
            PassageKind::Code
        };
    }
    a.end = b.end;
    a.line_end = b.line_end;
}

/// Which symbol names a merged passage: callables and types outrank
/// constants and macros.
fn rank(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Constant | SymbolKind::Macro => 0,
        SymbolKind::TypeAlias | SymbolKind::Schema => 1,
        SymbolKind::Module | SymbolKind::Impl => 2,
        _ => 3,
    }
}

#[cfg(test)]
#[path = "split_tests.rs"]
mod tests;
