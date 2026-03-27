//! HIR walker: decomposes a regex pattern into a `GramQuery` boolean tree.
//!
//! Follows Google codesearch's `analyze()` algorithm, adapted for sparse n-grams.
//!
//! # Key correctness rule
//!
//! `Repetition(min=0)` (optional subexpressions like `(foo)?`) contribute `All`
//! to their parent `And` node. After `GramQuery::simplify()`, `And` removes `All`
//! children, so optional prefixes/suffixes correctly contribute zero gram constraints.
//!
//! Example: `(foo)?bar`
//! - HIR: `Concat([Repetition(min=0, Capture(Literal("foo"))), Literal("bar")])`
//! - Walk: `And([All, Grams("bar")])`
//! - Simplify: removes `All` → `Grams("bar")`
//!
//! Do NOT change this to `And(Grams("foo"), Grams("bar"))`. That would be a
//! correctness bug: files with "bar" but not "foo" would be missed.

use regex_syntax::hir::{Hir, HirKind};

use crate::query::GramQuery;
use crate::tokenizer::build_covering_inner;

const MAX_LITERAL_VARIANTS: usize = 16;

/// Decompose a regex pattern into a `GramQuery` boolean tree.
///
/// Returns `Err` if the pattern fails regex syntax parsing.
/// The returned tree should be passed through `GramQuery::simplify()` before use.
pub fn decompose(pattern: &str, case_insensitive: bool) -> Result<GramQuery, String> {
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(case_insensitive)
        .build()
        .parse(pattern)
        .map_err(|e| e.to_string())?;
    Ok(walk(&hir))
}

/// Recursively walk an HIR node and produce a `GramQuery`.
fn walk(hir: &Hir) -> GramQuery {
    if let Some(query) = exact_literal_query(hir) {
        return query;
    }

    match hir.kind() {
        HirKind::Literal(lit) => {
            // lit.0 is Box<[u8]>: the literal bytes after regex_syntax normalization.
            // Use build_covering_inner (not build_covering) because regex literals
            // can end mid-token (e.g. "parse_quer" from `parse_quer[yi]`). Edge
            // grams at synthetic boundaries would cause false negatives.
            match build_covering_inner(&lit.0) {
                Some(grams) if !grams.is_empty() => GramQuery::Grams(grams),
                // No interior forced-boundary grams; fall back to full scan.
                _ => GramQuery::All,
            }
        }

        HirKind::Concat(subs) => {
            // All sub-patterns must match. Simplification later removes Any children.
            GramQuery::And(subs.iter().map(walk).collect())
        }

        HirKind::Alternation(subs) => {
            // Any branch may match. If any branch is All, the whole Or is All.
            GramQuery::Or(subs.iter().map(walk).collect())
        }

        HirKind::Repetition(rep) => {
            if rep.min >= 1 {
                // Required repetition: sub-pattern must match at least once.
                // Grams from the sub-pattern are valid constraints.
                walk(&rep.sub)
            } else {
                // Optional (min=0): sub-pattern may not appear at all.
                // Requiring its grams would produce false negatives.
                GramQuery::All
            }
        }

        HirKind::Capture(cap) => {
            // Capture groups don't change matching semantics; recurse into sub.
            walk(&cap.sub)
        }

        // Character classes, zero-width assertions, and empty patterns:
        // no grams can be extracted; the verifier handles them.
        HirKind::Class(_) | HirKind::Look(_) | HirKind::Empty => GramQuery::All,
    }
}

fn exact_literal_query(hir: &Hir) -> Option<GramQuery> {
    let literals = exact_literal_variants(hir, MAX_LITERAL_VARIANTS)?;
    let mut branches = Vec::with_capacity(literals.len());
    for literal in literals {
        let grams = build_covering_inner(&literal)?;
        if grams.is_empty() {
            return None;
        }
        branches.push(GramQuery::Grams(grams));
    }

    match branches.len() {
        0 => None,
        1 => branches.into_iter().next(),
        _ => Some(GramQuery::Or(branches)),
    }
}

fn exact_literal_variants(hir: &Hir, limit: usize) -> Option<Vec<Vec<u8>>> {
    match hir.kind() {
        HirKind::Empty => Some(vec![Vec::new()]),
        HirKind::Literal(lit) => Some(vec![lit.0.to_vec()]),
        HirKind::Capture(cap) => exact_literal_variants(&cap.sub, limit),
        HirKind::Concat(subs) => {
            let mut acc = vec![Vec::new()];
            for sub in subs {
                let variants = exact_literal_variants(sub, limit)?;
                acc = concat_variants(acc, variants, limit)?;
            }
            Some(acc)
        }
        HirKind::Alternation(subs) => {
            let mut acc = Vec::new();
            for sub in subs {
                let variants = exact_literal_variants(sub, limit)?;
                if acc.len() + variants.len() > limit {
                    return None;
                }
                acc.extend(variants);
            }
            Some(acc)
        }
        HirKind::Repetition(rep) => match rep.max {
            Some(max) if max == rep.min && max <= 4 => {
                let variants = exact_literal_variants(&rep.sub, limit)?;
                let mut acc = vec![Vec::new()];
                for _ in 0..rep.min {
                    acc = concat_variants(acc, variants.clone(), limit)?;
                }
                Some(acc)
            }
            _ => None,
        },
        HirKind::Class(_) | HirKind::Look(_) => None,
    }
}

fn concat_variants(left: Vec<Vec<u8>>, right: Vec<Vec<u8>>, limit: usize) -> Option<Vec<Vec<u8>>> {
    if left.is_empty() || right.is_empty() {
        return Some(Vec::new());
    }

    let total = left.len().checked_mul(right.len())?;
    if total > limit {
        return None;
    }

    let mut combined = Vec::with_capacity(total);
    for prefix in left {
        for suffix in &right {
            let mut literal = prefix.clone();
            literal.extend_from_slice(suffix);
            combined.push(literal);
        }
    }
    Some(combined)
}
