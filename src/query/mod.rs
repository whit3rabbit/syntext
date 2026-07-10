//! Query planning: GramQuery tree, query router, and cardinality-based ordering.
//!
//! The query router classifies a pattern into one of three execution paths:
//! - Literal: no regex metacharacters, use memchr::memmem for verification.
//! - IndexedRegex: HIR decomposition yields grams, use posting list intersection.
//! - FullScan: no extractable grams (e.g. `.*`), scan all candidate files.

/// Regex decomposition into n-gram boolean queries.
pub mod regex_decompose;

use crate::tokenizer::{build_covering, CoveringSet};

/// Boolean query tree produced by regex decomposition.
///
/// Execution semantics:
/// - `And`: all children must match (posting list intersection)
/// - `Or`: any child may match (posting list union)
/// - `Grams`: all hashes must appear in the document (implicit AND of posting lists)
/// - `All`: matches every document — fall back to full scan
/// - `None`: matches no document
#[derive(Debug, Clone)]
pub enum GramQuery {
    /// All children must match.
    And(Vec<GramQuery>),
    /// Any child may match.
    Or(Vec<GramQuery>),
    /// Gram hashes that all must appear in the document.
    Grams(Vec<u64>),
    /// Matches everything; requires full scan.
    All,
    /// Matches nothing.
    None,
}

impl GramQuery {
    /// Simplify the query tree by applying algebraic reduction rules.
    ///
    /// Rules applied recursively:
    /// - `And([..., All, ...])` → remove `All` children (All is identity for AND)
    /// - `Or([..., All, ...])` → `All` (All dominates OR)
    /// - `And([])` → `All`
    /// - `Or([])` → `None`
    /// - `And([x])` → `x`
    /// - `Or([x])` → `x`
    pub fn simplify(self) -> Self {
        match self {
            GramQuery::And(children) => {
                let simplified: Vec<GramQuery> = children
                    .into_iter()
                    .map(|c| c.simplify())
                    .filter(|c| !matches!(c, GramQuery::All))
                    .collect();
                match simplified.len() {
                    0 => GramQuery::All,
                    1 => simplified.into_iter().next().unwrap(),
                    _ => GramQuery::And(simplified),
                }
            }
            GramQuery::Or(children) => {
                let simplified: Vec<GramQuery> =
                    children.into_iter().map(|c| c.simplify()).collect();
                if simplified.iter().any(|c| matches!(c, GramQuery::All)) {
                    return GramQuery::All;
                }
                match simplified.len() {
                    0 => GramQuery::None,
                    1 => simplified.into_iter().next().unwrap(),
                    _ => GramQuery::Or(simplified),
                }
            }
            other => other,
        }
    }
}

/// Which execution path the search engine should use for a pattern.
#[derive(Debug, Clone)]
pub enum QueryRoute {
    /// No regex metacharacters. Use memchr::memmem for verification.
    Literal,
    /// HIR decomposition yielded at least one gram. Use posting list intersection
    /// followed by regex verification.
    IndexedRegex(GramQuery),
    /// No grams extractable (e.g. `.*`, single-char patterns). Scan all files.
    FullScan,
}

/// Classify a search pattern and return the optimal execution route.
///
/// - Literal if pattern has no regex metacharacters AND case-sensitive
/// - FullScan if HIR yields `All` (no useful grams)
/// - IndexedRegex otherwise
///
/// Symbol lookups are NOT routed here: they are an explicit `--sym` command
/// path (see `Index::search_symbols`), so `sym:`/`def:`/`ref:` are ordinary
/// searchable text.
pub fn route_query(pattern: &str, case_insensitive: bool) -> Result<QueryRoute, String> {
    if case_insensitive && is_literal(pattern) {
        // Mirror the case-sensitive `Literal` arm's candidate logic (see
        // search::search). Only the required (interior-anchored) grams are
        // AND-intersected: optional grams have synthetic boundaries and
        // ANDing them would silently drop sub-token matches (e.g. `st -i
        // parse` missing `reparse_input` when another file emits `parse` as a
        // token). With no required gram, route to full scan. Selectivity is
        // applied in search() so a common required gram still bails to scan.
        // Verification runs through the case-insensitive regex.
        return Ok(match build_covering(pattern.as_bytes()) {
            Some(covering) if !covering.required.is_empty() => {
                QueryRoute::IndexedRegex(GramQuery::Grams(covering.required))
            }
            _ => QueryRoute::FullScan,
        });
    }

    if !case_insensitive && is_literal(pattern) {
        return Ok(QueryRoute::Literal);
    }

    let gram_query = regex_decompose::decompose(pattern, case_insensitive)?;
    let gram_query = gram_query.simplify();

    Ok(match gram_query {
        GramQuery::All | GramQuery::None => QueryRoute::FullScan,
        q => QueryRoute::IndexedRegex(q),
    })
}

/// Returns `true` if the pattern contains no regex metacharacters.
///
/// Metacharacters: `. * + ? [ ] { } ( ) | ^ $ \`
pub fn is_literal(pattern: &str) -> bool {
    !pattern.chars().any(|c| {
        matches!(
            c,
            '.' | '*' | '+' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '|' | '^' | '$' | '\\'
        )
    })
}

/// Extract covering gram hashes from a literal pattern, classified by
/// boundary reliability.
///
/// Returns `None` if the pattern is too short to produce any qualifying gram.
pub fn literal_grams(pattern: &str) -> Option<CoveringSet> {
    build_covering(pattern.as_bytes())
}
