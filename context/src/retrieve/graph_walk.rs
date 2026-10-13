//! Entity-graph traversal: question entities to related evidence.
//!
//! Seeds are the question's n-grams that name an entity (directly or via an
//! alias). A beam search follows edges in both directions for a bounded
//! number of hops. Each step multiplies activation by the edge confidence,
//! a per-hop decay, a hub damping factor (an entity with hundreds of edges
//! says little about any one of them), and a bonus when the edge's predicate
//! shares a term with the question, so "who *founded* ..." prefers
//! `founded` edges over `related to` ones. Every traversed edge contributes
//! the passage it was read from, and every reached entity contributes the
//! passages that define it, so the model receives the whole chain of
//! evidence, not just its last hop.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::candidates::{TermStats, Visibility};
use super::query::AnalyzedQuery;
use crate::analysis::{entity_term, term_set};
use crate::error::Result;
use crate::graph::EdgeKind;
use crate::store::{PassageLoc, StoreSnapshot};

/// Traversal limits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WalkOptions {
    pub hops: u8,
    pub beam: usize,
    pub decay: f32,
    pub min_activation: f32,
    pub max_links: usize,
}

/// One relation step of an evidence chain, in the edge's own direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainStep {
    /// Subject entity, as written in the source.
    pub from: String,
    /// Predicate (`works for`, `calls`, `also known as`).
    pub relation: String,
    /// Object entity.
    pub to: String,
    /// Kind of relation; `None` for an alias hop.
    pub kind: Option<EdgeKind>,
    /// Confidence of the extracted relation, 0-1.
    pub confidence: f32,
}

/// How a passage was reached through the entity graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Chain {
    /// Relation hops from a question entity, in order.
    pub steps: Vec<ChainStep>,
    /// How the passage relates to the last entity: `states` (it is the
    /// source of the last step), `defines`, or `mentions`.
    pub link: String,
    /// The entity the passage defines or mentions.
    pub entity: String,
}

pub(crate) struct GraphHit {
    pub activation: f32,
    pub chain: Chain,
}

struct Reached {
    activation: f32,
    display: String,
    steps: Vec<ChainStep>,
}

const MAX_SEEDS: usize = 8;

pub(crate) fn walk(
    snap: &StoreSnapshot,
    query: &AnalyzedQuery,
    stats: &TermStats,
    vis: &Visibility,
    lexical: &HashMap<PassageLoc, f32>,
    opts: &WalkOptions,
) -> Result<(HashMap<PassageLoc, GraphHit>, Vec<String>)> {
    let mut hits: HashMap<PassageLoc, GraphHit> = HashMap::new();
    if opts.hops == 0 || !snap.segments.iter().any(|s| s.reader.graph.is_some()) {
        return Ok((hits, Vec::new()));
    }
    let query_terms: HashSet<&str> = query.content_terms.iter().map(String::as_str).collect();
    let mut best: HashMap<String, Reached> = seeds(snap, query, stats);
    let mut seed_names: Vec<String> = best.values().map(|r| r.display.clone()).collect();
    seed_names.sort();
    seed_names.dedup();
    let mut frontier: Vec<String> = best.keys().cloned().collect();
    frontier.sort();
    for _ in 0..opts.hops {
        let mut next: HashMap<String, Reached> = HashMap::new();
        for key in &frontier {
            let (act, steps) = match best.get(key) {
                Some(r) => (r.activation, r.steps.clone()),
                None => continue,
            };
            for_each_neighbor(snap, key, |n| {
                let bonus = if term_set(&n.relation)
                    .iter()
                    .any(|t| query_terms.contains(t.as_str()))
                {
                    2.5
                } else {
                    1.0
                };
                let a = act * n.confidence * opts.decay * bonus * n.damping;
                if a < opts.min_activation {
                    return;
                }
                let mut chain_steps = steps.clone();
                chain_steps.push(ChainStep {
                    from: n.subject_display.clone(),
                    relation: n.relation.clone(),
                    to: n.object_display.clone(),
                    kind: n.kind,
                    confidence: n.confidence,
                });
                if vis.passage(snap, n.evidence) {
                    let chain = Chain {
                        steps: chain_steps.clone(),
                        link: "states".to_string(),
                        entity: n.other_display.clone(),
                    };
                    upsert(&mut hits, n.evidence, a, chain);
                }
                let improves = best.get(&n.other_key).is_none_or(|r| r.activation < a)
                    && next.get(&n.other_key).is_none_or(|r| r.activation < a);
                if improves {
                    next.insert(
                        n.other_key.clone(),
                        Reached {
                            activation: a,
                            display: n.other_display.clone(),
                            steps: chain_steps,
                        },
                    );
                }
            });
        }
        let mut ranked: Vec<(String, Reached)> = next.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.activation
                .total_cmp(&a.1.activation)
                .then_with(|| a.0.cmp(&b.0))
        });
        ranked.truncate(opts.beam);
        frontier = ranked.iter().map(|(k, _)| k.clone()).collect();
        best.extend(ranked);
        if frontier.is_empty() {
            break;
        }
    }
    for (key, reached) in &best {
        link_passages(snap, key, reached, vis, lexical, opts, &mut hits)?;
    }
    Ok((hits, seed_names))
}

fn upsert(
    hits: &mut HashMap<PassageLoc, GraphHit>,
    loc: PassageLoc,
    activation: f32,
    chain: Chain,
) {
    match hits.get_mut(&loc) {
        Some(h) if h.activation >= activation => {}
        Some(h) => {
            h.activation = activation;
            h.chain = chain;
        }
        None => {
            hits.insert(loc, GraphHit { activation, chain });
        }
    }
}

/// Question n-grams that name an entity, by specificity. Longer n-grams
/// shadow the single words inside them.
fn seeds(
    snap: &StoreSnapshot,
    query: &AnalyzedQuery,
    stats: &TermStats,
) -> HashMap<String, Reached> {
    let max_idf = stats.max_idf().max(1.0);
    let mut out: Vec<(String, Reached)> = Vec::new();
    for key in &query.entity_keys {
        let shadowed = out.iter().any(|(k, _)| {
            let words: Vec<&str> = k.split(' ').collect();
            key.split(' ').all(|w| words.contains(&w))
        });
        if shadowed {
            continue;
        }
        let display = entity_display(snap, key);
        let aliased: Vec<(String, f32)> = snap
            .segments
            .iter()
            .flat_map(|s| {
                s.reader
                    .aliases_from(key)
                    .into_iter()
                    .map(|a| (s.reader.str(a.target).to_string(), a.confidence))
                    .collect::<Vec<_>>()
            })
            .collect();
        if display.is_none() && aliased.is_empty() {
            continue;
        }
        let specificity = if key.contains(' ') {
            1.0
        } else {
            entity_term(key).map_or(0.5, |t| (stats.idf(&t) / max_idf).clamp(0.3, 1.0))
        };
        if let Some(d) = display {
            out.push((
                key.clone(),
                Reached {
                    activation: specificity,
                    display: d,
                    steps: Vec::new(),
                },
            ));
        }
        for (target, conf) in aliased {
            let d = entity_display(snap, &target).unwrap_or_else(|| target.clone());
            out.push((
                target,
                Reached {
                    activation: specificity * conf,
                    display: d,
                    steps: Vec::new(),
                },
            ));
        }
    }
    out.sort_by(|a, b| b.1.activation.total_cmp(&a.1.activation));
    out.truncate(MAX_SEEDS);
    out.into_iter().collect()
}

fn entity_display(snap: &StoreSnapshot, key: &str) -> Option<String> {
    snap.segments.iter().find_map(|s| {
        let i = s.reader.entity_index(key)?;
        let g = s.reader.graph.as_ref()?;
        Some(s.reader.str(g.entities[i].display).to_string())
    })
}

/// A graph neighbour of an entity, via an edge or an alias.
struct Neighbor {
    other_key: String,
    other_display: String,
    subject_display: String,
    object_display: String,
    relation: String,
    kind: Option<EdgeKind>,
    confidence: f32,
    damping: f32,
    evidence: PassageLoc,
}

fn for_each_neighbor(snap: &StoreSnapshot, key: &str, mut f: impl FnMut(Neighbor)) {
    for (si, seg) in snap.segments.iter().enumerate() {
        let r = &seg.reader;
        let Some(g) = r.graph.as_ref() else {
            continue;
        };
        let disp = |i: u32| r.str(g.entities[i as usize].display).to_string();
        if let Some(e) = r.entity_index(key) {
            let degree = g.out_edges(e).len() + g.in_edges(e).count();
            let damping = 1.0 / (1.0 + (1.0 + degree as f32 / 8.0).ln());
            let outgoing = g.out_edges(e).iter().map(|x| (x, x.object));
            let incoming = g.in_edges(e).map(|x| (x, x.subject));
            for (edge, other) in outgoing.chain(incoming) {
                f(Neighbor {
                    other_key: r.str(g.entities[other as usize].key).to_string(),
                    other_display: disp(other),
                    subject_display: disp(edge.subject),
                    object_display: disp(edge.object),
                    relation: r.str(edge.predicate).to_string(),
                    kind: Some(edge.kind),
                    confidence: edge.confidence,
                    damping,
                    evidence: PassageLoc {
                        seg: si as u32,
                        pid: edge.passage,
                    },
                });
            }
        }
        let aliases = r
            .aliases_from(key)
            .into_iter()
            .map(|a| (a, a.target, a.alias));
        let reverse = r
            .aliases_to(key)
            .into_iter()
            .map(|a| (a, a.alias, a.target));
        for (a, other, this) in aliases.chain(reverse) {
            f(Neighbor {
                other_key: r.str(other).to_string(),
                other_display: r.str(other).to_string(),
                subject_display: r.str(this).to_string(),
                object_display: r.str(other).to_string(),
                relation: "also known as".to_string(),
                kind: None,
                confidence: a.confidence,
                damping: 1.0,
                evidence: PassageLoc {
                    seg: si as u32,
                    pid: a.passage,
                },
            });
        }
    }
}

/// Weight of a passage that mentions a reached entity without defining it.
const MENTION_WEIGHT: f32 = 0.5;
/// The same for an entity the question itself names: such a passage already
/// matched the name lexically, so a full mention weight would count that
/// match twice and lift every passing mention to the level of evidence the
/// graph actually found.
const SEED_MENTION_WEIGHT: f32 = 0.3;

/// Passages defining (weight 1) or mentioning a reached entity, preferring
/// passages that also matched lexically.
fn link_passages(
    snap: &StoreSnapshot,
    key: &str,
    reached: &Reached,
    vis: &Visibility,
    lexical: &HashMap<PassageLoc, f32>,
    opts: &WalkOptions,
    hits: &mut HashMap<PassageLoc, GraphHit>,
) -> Result<()> {
    for (si, seg) in snap.segments.iter().enumerate() {
        let r = &seg.reader;
        let (Some(g), Some(e)) = (r.graph.as_ref(), r.entity_index(key)) else {
            continue;
        };
        let mention_weight = if reached.steps.is_empty() {
            SEED_MENTION_WEIGHT
        } else {
            MENTION_WEIGHT
        };
        for (link, weight, pids) in [
            ("defines", 1.0, g.defs(e)?),
            ("mentions", mention_weight, g.mentions(e)?),
        ] {
            let mut locs: Vec<PassageLoc> = pids
                .into_iter()
                .map(|pid| PassageLoc {
                    seg: si as u32,
                    pid,
                })
                .filter(|&loc| vis.passage(snap, loc))
                .collect();
            locs.sort_by_key(|loc| !lexical.contains_key(loc));
            locs.truncate(opts.max_links);
            for loc in locs {
                let chain = Chain {
                    steps: reached.steps.clone(),
                    link: link.to_string(),
                    entity: reached.display.clone(),
                };
                upsert(hits, loc, reached.activation * weight, chain);
            }
        }
    }
    Ok(())
}
