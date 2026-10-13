//! Second-phase scoring: re-read each candidate's text and compute every
//! ranking feature exactly.

use std::collections::{HashMap, HashSet};

use memchr::memmem;

use super::candidates::{bm25, TermStats};
use super::graph_walk::Chain;
use super::query::{AnalyzedQuery, QueryIntent};
use super::{RankWeights, Signals};
use crate::analysis::{analyze, fold_key};
use crate::types::{DocKind, PassageKind};

/// Weight of a title (breadcrumb/symbol) occurrence in BM25F.
const TITLE_TF: f32 = 2.0;

/// What the reranker needs to know about a candidate.
pub(crate) struct View<'a> {
    pub text: &'a str,
    pub title: &'a str,
    pub symbol: Option<&'a str>,
    pub kind: PassageKind,
    pub doc_kind: DocKind,
    pub line_start: u32,
    pub seq: u64,
}

/// Signals gathered before reranking.
#[derive(Debug, Clone, Default)]
pub(crate) struct Prelim {
    pub lexical: f32,
    pub graph: f32,
    pub exact: bool,
    pub exact_lines: Vec<u32>,
    pub chain: Option<Chain>,
}

pub(crate) struct Scored {
    pub signals: Signals,
    pub bm25_raw: f32,
    pub matched: Vec<String>,
    pub focus_lines: Vec<u32>,
}

pub(crate) fn features(
    v: &View<'_>,
    q: &AnalyzedQuery,
    stats: &TermStats,
    intent: QueryIntent,
    prelim: &Prelim,
    max_seq: u64,
) -> Scored {
    let content: HashMap<&str, usize> = q
        .content_terms
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();
    let compounds: HashSet<&str> = q.compounds.iter().map(String::as_str).collect();
    let mut tf: HashMap<&str, f32> = HashMap::new();
    let mut positions: Vec<(u32, usize)> = Vec::new();
    let mut focus_bytes: Vec<usize> = Vec::new();
    let mut length = 0u32;
    let terms = analyze(v.text);
    for t in &terms {
        if !t.compound {
            length += 1;
        }
        if let Some((&k, &i)) = content.get_key_value(t.text.as_str()) {
            *tf.entry(k).or_insert(0.0) += 1.0;
            positions.push((t.pos, i));
            focus_bytes.push(t.start);
        } else if let Some(&k) = compounds.get(t.text.as_str()) {
            *tf.entry(k).or_insert(0.0) += 1.0;
            focus_bytes.push(t.start);
        }
    }
    let title_terms: HashSet<String> = analyze(v.title).into_iter().map(|t| t.text).collect();

    let mut bm25_raw = 0.0;
    for t in q.all_terms() {
        let f = tf.get(t.as_str()).copied().unwrap_or(0.0)
            + if title_terms.contains(t) {
                TITLE_TF
            } else {
                0.0
            };
        bm25_raw += bm25(f, length as f32, stats.avgdl, stats.idf(t));
    }

    // Coverage over the question's concepts; identifiers stand in when the
    // question has no plain words.
    let cover_terms: Vec<&String> = if q.content_terms.is_empty() {
        q.compounds.iter().collect()
    } else {
        q.content_terms.iter().collect()
    };
    let total_idf: f32 = cover_terms.iter().map(|t| stats.idf(t)).sum();
    let in_body_or_title = |t: &str| tf.contains_key(t) || title_terms.contains(t);
    let covered: f32 = cover_terms
        .iter()
        .filter(|t| in_body_or_title(t))
        .map(|t| stats.idf(t))
        .sum();
    let in_title: f32 = cover_terms
        .iter()
        .filter(|t| title_terms.contains(t.as_str()))
        .map(|t| stats.idf(t))
        .sum();
    let ratio = |x: f32| {
        if total_idf > 0.0 {
            (x / total_idf).min(1.0)
        } else {
            0.0
        }
    };

    let lower = v.text.to_lowercase();
    let exact_phrases = q
        .phrases
        .iter()
        .filter(|p| memmem::find(lower.as_bytes(), p.as_bytes()).is_some())
        .count();
    let bigrams = bigram_fraction(&positions, q.content_terms.len());
    let phrase = if q.phrases.is_empty() {
        bigrams
    } else {
        0.6 * exact_phrases as f32 / q.phrases.len() as f32 + 0.4 * bigrams
    };

    let signals = Signals {
        bm25: 0.0,
        coverage: ratio(covered),
        proximity: proximity(&mut positions, q.content_terms.len()),
        phrase,
        title: ratio(in_title),
        symbol: symbol_match(v.symbol, q),
        exact: if prelim.exact { 1.0 } else { 0.0 },
        graph: prelim.graph.clamp(0.0, 1.0),
        structure: structure_prior(intent, v.kind),
        recency: if v.doc_kind == DocKind::Turn {
            (-((max_seq.saturating_sub(v.seq)) as f32) / 40.0).exp()
        } else {
            0.0
        },
    };

    let mut focus_lines: Vec<u32> = focus_bytes
        .iter()
        .map(|&b| v.line_start + v.text[..b].bytes().filter(|&c| c == b'\n').count() as u32)
        .chain(prelim.exact_lines.iter().copied())
        .collect();
    focus_lines.sort_unstable();
    focus_lines.dedup();
    let matched = q
        .all_terms()
        .filter(|t| tf.contains_key(t.as_str()))
        .cloned()
        .collect();
    Scored {
        signals,
        bm25_raw,
        matched,
        focus_lines,
    }
}

/// Weighted sum of normalized signals; zero when nothing ties the passage
/// to the question (no term, phrase, exact match, or graph link).
pub(crate) fn combine(s: &Signals, w: &RankWeights, intent: QueryIntent) -> f32 {
    if s.coverage == 0.0 && s.exact == 0.0 && s.graph == 0.0 && s.phrase == 0.0 {
        return 0.0;
    }
    let recency_weight = if intent == QueryIntent::Recall {
        w.recency * 3.0
    } else {
        w.recency
    };
    // "Where is X defined?" is a question about structure: the definition
    // must beat prose that merely names X, however closely that prose
    // echoes the question.
    let (symbol_weight, structure_weight) = if intent == QueryIntent::Locate {
        (w.symbol * 2.0, w.structure * 2.0)
    } else {
        (w.symbol, w.structure)
    };
    w.bm25 * s.bm25
        + w.coverage * s.coverage
        + w.proximity * s.proximity
        + w.phrase * s.phrase
        + w.title * s.title
        + symbol_weight * s.symbol
        + w.exact * s.exact
        + w.graph * s.graph
        + structure_weight * s.structure
        + recency_weight * s.recency
}

/// Stems that make a question about tests ("tests", "testing", "specs",
/// "fixtures", "mocks").
const TEST_TERMS: &[&str] = &["fixtur", "mock", "spec", "test", "unittest"];

/// True when the question is about tests, so test code competes on equal
/// terms with the code under test.
pub(crate) fn asks_about_tests(q: &AnalyzedQuery) -> bool {
    q.content_terms
        .iter()
        .any(|t| TEST_TERMS.contains(&t.as_str()))
}

/// Score multiplier for where a document lives. Vendored and generated
/// code rarely answers a question about the project itself, and tests
/// repeat the names they exercise, so for "where is X" they would crowd out
/// X's definition unless the question asks about tests.
pub(crate) fn path_prior(path: Option<&str>, wants_tests: bool) -> f32 {
    let Some(path) = path else {
        return 1.0;
    };
    let mut prior = 1.0;
    if is_vendored(path) {
        prior *= 0.75;
    }
    if !wants_tests && is_test_path(path) {
        prior *= 0.8;
    }
    prior
}

/// Test code: under a test directory or named like a test file.
fn is_test_path(path: &str) -> bool {
    let p = format!("/{path}");
    let in_test_dir = [
        "/tests/",
        "/test/",
        "/__tests__/",
        "/__mocks__/",
        "/spec/",
        "/specs/",
        "/testdata/",
    ]
    .iter()
    .any(|d| p.contains(d));
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    in_test_dir
        || matches!(stem, "test" | "tests" | "conftest")
        || stem.starts_with("test_")
        || ["_test", "_tests", "_spec"]
            .iter()
            .any(|s| stem.ends_with(s))
        || name.contains(".test.")
        || name.contains(".spec.")
        || (stem.len() > 4 && (stem.ends_with("Test") || stem.ends_with("Tests")))
}

fn is_vendored(path: &str) -> bool {
    let p = format!("/{path}");
    [
        "/vendor/",
        "/node_modules/",
        "/third_party/",
        "/dist/",
        "/build/",
        "/generated/",
    ]
    .iter()
    .any(|d| p.contains(d))
        || path.contains(".min.")
}

/// Smallest token window containing every distinct matched question term:
/// `(k / n) * sqrt(k / width)`, 0 when fewer than two terms match.
fn proximity(positions: &mut [(u32, usize)], n: usize) -> f32 {
    if n < 2 || positions.len() < 2 {
        return 0.0;
    }
    positions.sort_unstable();
    let mut counts = vec![0u32; n];
    let k = {
        let mut seen = vec![false; n];
        positions
            .iter()
            .filter(|&&(_, i)| !std::mem::replace(&mut seen[i], true))
            .count()
    };
    if k < 2 {
        return 0.0;
    }
    let (mut have, mut lo, mut best) = (0usize, 0usize, u32::MAX);
    for hi in 0..positions.len() {
        let i = positions[hi].1;
        if counts[i] == 0 {
            have += 1;
        }
        counts[i] += 1;
        while have == k {
            best = best.min(positions[hi].0 - positions[lo].0 + 1);
            let j = positions[lo].1;
            counts[j] -= 1;
            if counts[j] == 0 {
                have -= 1;
            }
            lo += 1;
        }
    }
    let width = best.max(k as u32) as f32;
    (k as f32 / n as f32) * (k as f32 / width).sqrt()
}

/// Fraction of consecutive question-term pairs that appear in order within
/// one stopword of each other.
fn bigram_fraction(positions: &[(u32, usize)], n: usize) -> f32 {
    if n < 2 {
        return 0.0;
    }
    let mut by_term: HashMap<usize, Vec<u32>> = HashMap::new();
    for &(p, i) in positions {
        by_term.entry(i).or_default().push(p);
    }
    let hits = (0..n - 1)
        .filter(|&i| {
            let (Some(a), Some(b)) = (by_term.get(&i), by_term.get(&(i + 1))) else {
                return false;
            };
            a.iter()
                .any(|&pa| b.iter().any(|&pb| pb > pa && pb - pa <= 2))
        })
        .count();
    hits as f32 / (n - 1) as f32
}

fn symbol_match(symbol: Option<&str>, q: &AnalyzedQuery) -> f32 {
    let Some(sym) = symbol.filter(|s| !s.is_empty()) else {
        return 0.0;
    };
    let key = fold_key(sym);
    if q.entity_keys.contains(&key) {
        return 1.0;
    }
    let words: Vec<String> = analyze(sym)
        .into_iter()
        .filter(|t| !t.compound)
        .map(|t| t.text)
        .collect();
    if !words.is_empty() && words.iter().all(|w| q.content_terms.contains(w)) {
        0.6
    } else {
        0.0
    }
}

fn structure_prior(intent: QueryIntent, kind: PassageKind) -> f32 {
    use PassageKind::*;
    match (intent, kind) {
        (QueryIntent::Locate, Definition) => 1.0,
        (QueryIntent::Locate, Code) => 0.6,
        (QueryIntent::Locate, Data) => 0.4,
        (QueryIntent::Locate, _) => 0.25,
        (QueryIntent::Explain, Prose) => 1.0,
        (QueryIntent::Explain, Definition) => 0.7,
        (QueryIntent::Explain, Turn) => 0.5,
        (QueryIntent::Explain, _) => 0.45,
        (QueryIntent::Fact, Prose) => 1.0,
        (QueryIntent::Fact, Turn) => 0.8,
        (QueryIntent::Fact, Data) => 0.6,
        (QueryIntent::Fact, _) => 0.35,
        (QueryIntent::Recall, Turn) => 1.0,
        (QueryIntent::Recall, Prose) => 0.4,
        (QueryIntent::Recall, _) => 0.3,
        (QueryIntent::General, Code | Data) => 0.5,
        (QueryIntent::General, _) => 0.7,
    }
}

#[cfg(test)]
#[path = "rerank_tests.rs"]
mod tests;
