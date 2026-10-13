//! The retrieval pipeline: candidates from every source, one reranker.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use xxhash_rust::xxh64::xxh64;

use super::access::DocAccess;
use super::candidates::{lexical, TermStats, Visibility};
use super::graph_walk::{walk, WalkOptions};
use super::query::{analyze_query, QueryIntent};
use super::rerank::{asks_about_tests, combine, features, path_prior, Prelim, View};
use super::syntext_source::{ExactResults, LiveHit, SyntextSource};
use super::{QueryReport, RankedPassage, RetrieveOptions, Retrieved, Signals, StageTimings};
use crate::compile::CompileOptions;
use crate::error::Result;
use crate::store::{PassageLoc, StoreSnapshot};
use crate::types::{DocKind, PassageKind};

fn micros(t: Instant) -> u64 {
    t.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

/// Retrieve ranked passages for `question` from `snap` (and `syntext`).
pub(crate) fn retrieve(
    snap: &Arc<StoreSnapshot>,
    syntext: Option<&SyntextSource>,
    compile: &CompileOptions,
    question: &str,
    opts: &RetrieveOptions,
) -> Result<Retrieved> {
    let start = Instant::now();
    let mut timings = StageTimings::default();
    let q = analyze_query(question);
    let intent = opts.intent.or(q.intent).unwrap_or(QueryIntent::General);
    let stats = TermStats::new(snap, q.all_terms());
    let vis = Visibility::new(snap, &opts.scopes, &opts.exclude);
    timings.analyze_us = micros(start);

    let t = Instant::now();
    let lex = lexical(
        snap,
        &q,
        &stats,
        &vis,
        opts.postings_budget,
        opts.candidate_limit,
    )?;
    timings.lexical_us = micros(t);

    let t = Instant::now();
    let walk_opts = WalkOptions {
        hops: opts.hops,
        beam: opts.beam.max(1),
        decay: 0.6,
        min_activation: 0.02,
        max_links: 12,
    };
    let (graph, seeds) = walk(snap, &q, &stats, &vis, &lex, &walk_opts)?;
    timings.graph_us = micros(t);

    let t = Instant::now();
    let exact = match syntext {
        Some(s) => s.exact(&q, snap, compile, &vis, &opts.scopes, &opts.exclude)?,
        None => ExactResults::default(),
    };
    timings.exact_us = micros(t);

    let t = Instant::now();
    let max_lex = lex
        .values()
        .copied()
        .fold(0.0f32, f32::max)
        .max(f32::MIN_POSITIVE);
    let mut pool: HashMap<PassageLoc, Prelim> = HashMap::new();
    for (loc, s) in &lex {
        pool.entry(*loc).or_default().lexical = s / max_lex;
    }
    for (loc, hit) in graph {
        let p = pool.entry(loc).or_default();
        p.graph = hit.activation;
        p.chain = Some(hit.chain);
    }
    for (loc, lines) in exact.stored {
        let p = pool.entry(loc).or_default();
        p.exact = true;
        p.exact_lines = lines;
    }
    let mut entries: Vec<(PassageLoc, Prelim)> = pool.into_iter().collect();
    let prelim_score = |p: &Prelim| p.lexical + p.graph + if p.exact { 1.0 } else { 0.0 };
    entries.sort_by(|a, b| {
        prelim_score(&b.1)
            .total_cmp(&prelim_score(&a.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    entries.truncate(opts.rerank_limit.max(1));

    let live_keys: HashSet<&str> = exact.live.iter().map(|h| h.doc.key.as_str()).collect();
    let mut candidates: Vec<(RankedPassage, Prelim)> =
        Vec::with_capacity(entries.len() + exact.live.len());
    for (loc, prelim) in entries {
        let passage = stored_passage(snap, loc)?;
        // A stale stored copy loses to the live file syntext just verified.
        if !live_keys.contains(passage.doc_key.as_str()) {
            candidates.push((passage, prelim));
        }
    }
    for hit in &exact.live {
        candidates.push(live_passage(hit));
    }
    let considered = candidates.len();
    let max_seq = candidates
        .iter()
        .filter(|(p, _)| p.doc_kind == DocKind::Turn)
        .map(|(p, _)| p.seq)
        .max()
        .unwrap_or(0);

    let mut scored: Vec<(RankedPassage, f32)> = candidates
        .into_par_iter()
        .map(|(mut p, prelim)| {
            let title = match &p.symbol {
                Some(s) => format!("{} {s}", p.breadcrumb),
                None => p.breadcrumb.clone(),
            };
            let view = View {
                text: &p.text,
                title: &title,
                symbol: p.symbol.as_deref(),
                kind: p.kind,
                doc_kind: p.doc_kind,
                line_start: p.lines.0,
                seq: p.seq,
            };
            let s = features(&view, &q, &stats, intent, &prelim, max_seq);
            p.signals = s.signals;
            p.matched_terms = s.matched;
            p.focus_lines = s.focus_lines;
            p.chain = prelim.chain;
            (p, s.bm25_raw)
        })
        .collect();
    let max_bm25 = scored
        .iter()
        .map(|(_, b)| *b)
        .fold(0.0f32, f32::max)
        .max(f32::MIN_POSITIVE);
    let wants_tests = asks_about_tests(&q);
    for (p, raw) in &mut scored {
        p.signals.bm25 = *raw / max_bm25;
        p.score =
            combine(&p.signals, &opts.weights, intent) * path_prior(p.path.as_deref(), wants_tests);
    }
    let mut passages: Vec<RankedPassage> = scored
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| p.score > 0.0)
        .collect();
    passages.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.doc_key.cmp(&b.doc_key))
            .then_with(|| a.byte_range.start.cmp(&b.byte_range.start))
    });
    passages.truncate(opts.max_results);
    timings.rerank_us = micros(t);
    timings.total_us = micros(start);

    Ok(Retrieved {
        generation: snap.generation(),
        store_id: snap.store_id().to_string(),
        query: QueryReport {
            intent,
            terms: q.all_terms().cloned().collect(),
            identifiers: q.identifiers.clone(),
            entities: seeds,
        },
        passages,
        candidates_considered: considered,
        timings,
    })
}

/// Stable identifier of a passage version: changes when the source or the
/// passage boundaries change.
fn passage_id(key: &str, source_hash: &str, start: usize, end: usize) -> String {
    let mut buf = Vec::with_capacity(key.len() + source_hash.len() + 16);
    buf.extend_from_slice(key.as_bytes());
    buf.push(0);
    buf.extend_from_slice(source_hash.as_bytes());
    buf.extend_from_slice(&(start as u64).to_le_bytes());
    buf.extend_from_slice(&(end as u64).to_le_bytes());
    format!("{:016x}", xxh64(&buf, 0))
}

fn stored_passage(snap: &Arc<StoreSnapshot>, loc: PassageLoc) -> Result<RankedPassage> {
    let r = snap.reader(loc.seg);
    let p = &r.passages[loc.pid as usize];
    let d = &r.docs[p.doc as usize];
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let summary = snap.summary(crate::store::DocLoc {
        seg: loc.seg,
        doc: p.doc,
    });
    let (start, end) = (p.start as usize, p.end as usize);
    Ok(RankedPassage {
        passage_id: passage_id(&summary.key, &summary.source_hash, start, end),
        doc_key: summary.key,
        title: summary.title,
        path: summary.path,
        breadcrumb: r.str(p.breadcrumb).to_string(),
        symbol: opt(r.str(p.symbol)),
        kind: p.kind,
        doc_kind: d.kind,
        trust: d.trust,
        scope: summary.scope,
        role: d.role,
        byte_range: start..end,
        lines: (p.line_start, p.line_end),
        text: r.passage_text(loc.pid)?,
        score: 0.0,
        signals: Signals::default(),
        matched_terms: Vec::new(),
        focus_lines: Vec::new(),
        chain: None,
        source_hash: summary.source_hash,
        live: false,
        access: DocAccess::Stored {
            snap: Arc::clone(snap),
            seg: loc.seg,
            doc: p.doc,
        },
        index_in_doc: (loc.pid - d.first_passage) as usize,
        anchor: (p.anchor_offset, p.anchor_len),
        seq: d.seq,
    })
}

fn live_passage(hit: &LiveHit) -> (RankedPassage, Prelim) {
    let doc = &hit.doc;
    let p = &doc.layout.passages[hit.passage];
    let title = doc.path.rsplit('/').next().unwrap_or(&doc.path).to_string();
    let passage = RankedPassage {
        passage_id: passage_id(&doc.key, &doc.source_hash, p.start, p.end),
        doc_key: doc.key.clone(),
        title,
        path: Some(doc.path.clone()),
        breadcrumb: p.breadcrumb.clone(),
        symbol: p.symbol.as_ref().map(|s| s.name.clone()),
        kind: p.kind,
        doc_kind: DocKind::File,
        trust: doc.trust,
        scope: doc.scope.clone(),
        role: None,
        byte_range: p.start..p.end,
        lines: (p.line_start, p.line_end),
        text: doc.text[p.start..p.end].to_string(),
        score: 0.0,
        signals: Signals::default(),
        matched_terms: Vec::new(),
        focus_lines: Vec::new(),
        chain: None,
        source_hash: doc.source_hash.clone(),
        live: true,
        access: DocAccess::Live(Arc::clone(doc)),
        index_in_doc: hit.passage,
        anchor: (p.anchor_offset, p.anchor_len),
        seq: 0,
    };
    let prelim = Prelim {
        exact: true,
        exact_lines: hit.lines.clone(),
        ..Prelim::default()
    };
    debug_assert!(passage.kind != PassageKind::Turn);
    (passage, prelim)
}
