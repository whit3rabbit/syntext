//! First-phase candidates: BM25 over the inverted index, rarest terms
//! first, under a postings budget.
//!
//! Candidate generation only has to *find* the right passages; scoring them
//! precisely is the reranker's job (it re-reads candidate text and computes
//! every feature, including the contribution of common terms skipped here).
//! So once the rarest terms are traversed, a very common term is skipped
//! when its posting list would blow the budget.

use std::collections::HashMap;

use super::query::AnalyzedQuery;
use crate::error::Result;
use crate::store::{PassageLoc, StoreSnapshot};
use crate::types::{DocKey, ScopeFilter};

/// BM25 saturation.
pub(crate) const K1: f32 = 1.2;
/// BM25 length normalization; mild, because segmentation already bounds
/// passage size.
pub(crate) const B: f32 = 0.6;

/// One BM25 term contribution.
pub(crate) fn bm25(tf: f32, len: f32, avgdl: f32, idf: f32) -> f32 {
    if tf <= 0.0 {
        return 0.0;
    }
    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avgdl))
}

/// Which documents a query may see: live, scope-granted, not excluded.
pub(crate) struct Visibility {
    allowed: Vec<Vec<bool>>,
}

impl Visibility {
    pub(crate) fn new(snap: &StoreSnapshot, scopes: &ScopeFilter, exclude: &[DocKey]) -> Self {
        let mut allowed = Vec::with_capacity(snap.segments.len());
        for (si, seg) in snap.segments.iter().enumerate() {
            let r = &seg.reader;
            // Scope strings are deduplicated per segment, so the offset
            // identifies the scope: one string comparison per distinct scope.
            let mut by_scope: HashMap<u32, bool> = HashMap::new();
            let v: Vec<bool> = r
                .docs
                .iter()
                .enumerate()
                .map(|(di, d)| {
                    snap.doc_live(si as u32, di as u32)
                        && *by_scope
                            .entry(d.scope.off)
                            .or_insert_with(|| scopes.allows(r.str(d.scope)))
                })
                .collect();
            allowed.push(v);
        }
        for key in exclude {
            if let Some(loc) = snap.lookup(key.as_str()) {
                allowed[loc.seg as usize][loc.doc as usize] = false;
            }
        }
        Visibility { allowed }
    }

    pub(crate) fn doc(&self, seg: u32, doc: u32) -> bool {
        self.allowed
            .get(seg as usize)
            .and_then(|v| v.get(doc as usize))
            .copied()
            .unwrap_or(false)
    }

    pub(crate) fn passage(&self, snap: &StoreSnapshot, loc: PassageLoc) -> bool {
        self.doc(loc.seg, snap.passage(loc).doc)
    }
}

/// Corpus statistics for the query's terms.
pub(crate) struct TermStats {
    pub n: f32,
    pub avgdl: f32,
    df: HashMap<String, u64>,
}

impl TermStats {
    pub(crate) fn new<'a>(snap: &StoreSnapshot, terms: impl Iterator<Item = &'a String>) -> Self {
        let df = terms.map(|t| (t.clone(), snap.df(t))).collect();
        TermStats {
            n: snap.passage_count().max(1) as f32,
            avgdl: snap.avg_len(),
            df,
        }
    }

    pub(crate) fn df(&self, term: &str) -> u64 {
        self.df.get(term).copied().unwrap_or(0)
    }

    /// IDF of a term that occurs nowhere (the upper bound).
    pub(crate) fn max_idf(&self) -> f32 {
        (1.0 + (self.n + 0.5) / 0.5).ln()
    }

    /// BM25 IDF, always positive.
    pub(crate) fn idf(&self, term: &str) -> f32 {
        let df = self.df(term) as f32;
        let n = self.n.max(df);
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }
}

/// BM25-scored candidates, best `limit` first.
pub(crate) fn lexical(
    snap: &StoreSnapshot,
    query: &AnalyzedQuery,
    stats: &TermStats,
    vis: &Visibility,
    postings_budget: usize,
    limit: usize,
) -> Result<HashMap<PassageLoc, f32>> {
    let mut terms: Vec<(&String, u64)> = query
        .all_terms()
        .map(|t| (t, stats.df(t)))
        .filter(|&(_, df)| df > 0)
        .collect();
    terms.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
    let mut scores: HashMap<PassageLoc, f32> = HashMap::new();
    let mut traversed = 0usize;
    for (i, &(term, df)) in terms.iter().enumerate() {
        // The two rarest terms are always traversed so that a question
        // made only of common words still yields candidates.
        if i >= 2 && traversed.saturating_add(df as usize) > postings_budget {
            continue;
        }
        let idf = stats.idf(term);
        for (si, seg) in snap.segments.iter().enumerate() {
            let r = &seg.reader;
            let Some(ti) = r.term_index(term) else {
                continue;
            };
            for (pid, tf) in r.postings(ti)? {
                let p = &r.passages[pid as usize];
                if !vis.doc(si as u32, p.doc) {
                    continue;
                }
                let loc = PassageLoc {
                    seg: si as u32,
                    pid,
                };
                *scores.entry(loc).or_insert(0.0) +=
                    bm25(tf as f32, p.length as f32, stats.avgdl, idf);
            }
        }
        traversed = traversed.saturating_add(df as usize);
    }
    if scores.len() > limit {
        let mut v: Vec<(PassageLoc, f32)> = scores.into_iter().collect();
        v.select_nth_unstable_by(limit, |a, b| {
            b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))
        });
        v.truncate(limit);
        scores = v.into_iter().collect();
    }
    Ok(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm25_saturates_and_rewards_rarity() {
        let common = bm25(1.0, 10.0, 10.0, 0.1);
        let rare = bm25(1.0, 10.0, 10.0, 5.0);
        assert!(rare > common);
        let tf1 = bm25(1.0, 10.0, 10.0, 1.0);
        let tf10 = bm25(10.0, 10.0, 10.0, 1.0);
        assert!(tf10 > tf1 && tf10 < tf1 * (K1 + 1.0) * 1.01);
        assert_eq!(bm25(0.0, 10.0, 10.0, 1.0), 0.0);
        assert!(
            bm25(1.0, 5.0, 10.0, 1.0) > bm25(1.0, 50.0, 10.0, 1.0),
            "shorter wins"
        );
    }
}
