//! Graph sections of a segment: entities, edges, aliases, and the passage
//! lists behind definitions and mentions.
//!
//! Entities are stored sorted by canonical key, so a reader finds one with a
//! binary search and joins across segments by key. Edges are sorted by
//! subject (each entity record points at its run of outgoing edges);
//! `EDGES_BY_OBJECT` is a permutation sorted by object for reverse
//! traversal.

use std::collections::BTreeMap;

use super::format::*;
use super::writer::{to_u32, Strings};
use crate::compile::CompiledDoc;
use crate::error::Result;
use crate::graph::{EdgeKind, EntityKind};

pub(crate) struct GraphSections {
    pub entities_buf: Vec<u8>,
    pub edges_buf: Vec<u8>,
    pub by_object_buf: Vec<u8>,
    pub aliases_buf: Vec<u8>,
    pub lists_buf: Vec<u8>,
    pub entities: u32,
    pub edges: u32,
    pub aliases: u32,
}

struct EntAgg<'a> {
    display: &'a str,
    kind: EntityKind,
    defs: Vec<u32>,
    mentions: Vec<u32>,
}

struct RawEdge<'a> {
    subject: &'a str,
    object: &'a str,
    predicate: &'a str,
    passage: u32,
    confidence: f32,
    kind: EdgeKind,
}

/// Scale a confidence in `[0, 1]` to the stored `u16` (1e-4 resolution).
pub(crate) fn conf_to_u16(c: f32) -> u16 {
    (c.clamp(0.0, 1.0) * 10_000.0).round() as u16
}

pub(crate) fn conf_from_u16(c: u16) -> f32 {
    f32::from(c.min(10_000)) / 10_000.0
}

pub(crate) fn encode_graph(docs: &[CompiledDoc], strings: &mut Strings) -> Result<GraphSections> {
    let mut ents: BTreeMap<&str, EntAgg<'_>> = BTreeMap::new();
    let mut raw_edges: Vec<RawEdge<'_>> = Vec::new();
    let mut raw_aliases: Vec<(&str, &str, u32, f32)> = Vec::new();
    let mut pid: u32 = 0;
    for d in docs {
        for p in &d.passages {
            let f = &p.facts;
            for e in &f.defines {
                agg(&mut ents, &e.key, &e.display, e.kind).defs.push(pid);
            }
            for e in &f.mentions {
                agg(&mut ents, &e.key, &e.display, e.kind)
                    .mentions
                    .push(pid);
            }
            for e in &f.edges {
                agg(
                    &mut ents,
                    &e.subject.key,
                    &e.subject.display,
                    e.subject.kind,
                );
                agg(&mut ents, &e.object.key, &e.object.display, e.object.kind);
                raw_edges.push(RawEdge {
                    subject: &e.subject.key,
                    object: &e.object.key,
                    predicate: &e.predicate,
                    passage: pid,
                    confidence: e.confidence,
                    kind: e.kind,
                });
            }
            for a in &f.aliases {
                raw_aliases.push((&a.alias, &a.target, pid, a.confidence));
            }
            pid += 1;
        }
    }

    let index: BTreeMap<&str, u32> = ents
        .keys()
        .enumerate()
        .map(|(i, k)| (*k, i as u32))
        .collect();
    let mut edges: Vec<(u32, u32, &str, u32, u16, EdgeKind)> = raw_edges
        .iter()
        .map(|e| {
            (
                index[e.subject],
                index[e.object],
                e.predicate,
                e.passage,
                conf_to_u16(e.confidence),
                e.kind,
            )
        })
        .collect();
    edges.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
    edges.dedup_by(|a, b| (a.0, a.1, a.2, a.3) == (b.0, b.1, b.2, b.3));

    let mut edges_buf = Enc::default();
    let mut edge_ranges = vec![(0u32, 0u32); ents.len()];
    for (i, e) in edges.iter().enumerate() {
        let r = &mut edge_ranges[e.0 as usize];
        if r.1 == 0 {
            r.0 = i as u32;
        }
        r.1 += 1;
        edges_buf.u32(e.0);
        edges_buf.u32(e.1);
        edges_buf.str_ref(strings.add(e.2)?);
        edges_buf.u32(e.3);
        edges_buf.u16(e.4);
        edges_buf.u8(e.5.code());
        edges_buf.u8(0);
    }
    let mut by_object: Vec<u32> = (0..to_u32(edges.len(), "edges")?).collect();
    by_object.sort_by_key(|&i| (edges[i as usize].1, edges[i as usize].0, i));
    let mut by_object_buf = Enc::default();
    for i in by_object {
        by_object_buf.u32(i);
    }

    let mut lists = Vec::new();
    let mut entities_buf = Enc::default();
    for (i, (key, e)) in ents.iter_mut().enumerate() {
        e.defs.dedup();
        e.mentions.dedup();
        entities_buf.str_ref(strings.add(key)?);
        entities_buf.str_ref(strings.add(e.display)?);
        entities_buf.u8(e.kind.code());
        entities_buf.u8(0);
        entities_buf.u16(0);
        for list in [&e.defs, &e.mentions] {
            entities_buf.u32(to_u32(lists.len(), "graph list bytes")?);
            entities_buf.u32(to_u32(list.len(), "graph list entries")?);
            put_delta_list(&mut lists, list);
        }
        entities_buf.u32(edge_ranges[i].0);
        entities_buf.u32(edge_ranges[i].1);
        entities_buf.u32(0);
    }

    raw_aliases.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    raw_aliases.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let mut aliases_buf = Enc::default();
    for (alias, target, passage, conf) in &raw_aliases {
        aliases_buf.str_ref(strings.add(alias)?);
        aliases_buf.str_ref(strings.add(target)?);
        aliases_buf.u32(*passage);
        aliases_buf.u16(conf_to_u16(*conf));
        aliases_buf.u16(0);
    }

    Ok(GraphSections {
        entities: to_u32(ents.len(), "entities")?,
        edges: to_u32(edges.len(), "edges")?,
        aliases: to_u32(raw_aliases.len(), "aliases")?,
        entities_buf: entities_buf.buf,
        edges_buf: edges_buf.buf,
        by_object_buf: by_object_buf.buf,
        aliases_buf: aliases_buf.buf,
        lists_buf: lists,
    })
}

fn agg<'m, 'a>(
    ents: &'m mut BTreeMap<&'a str, EntAgg<'a>>,
    key: &'a str,
    display: &'a str,
    kind: EntityKind,
) -> &'m mut EntAgg<'a> {
    ents.entry(key).or_insert_with(|| EntAgg {
        display,
        kind,
        defs: Vec::new(),
        mentions: Vec::new(),
    })
}
