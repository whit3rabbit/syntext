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
