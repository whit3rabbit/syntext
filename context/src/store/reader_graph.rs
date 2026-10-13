//! Graph tables of a segment: entities, edges, reverse edge index, aliases.

use super::format::*;
use super::reader_records::check_str;
use super::writer_graph::conf_from_u16;
use crate::error::{ContextError, Result};
use crate::graph::{EdgeKind, EntityKind};

#[derive(Debug, Clone)]
pub(crate) struct EntityRec {
    pub key: StrRef,
    pub display: StrRef,
    defs: (u32, u32),
    mentions: (u32, u32),
    edge_first: u32,
    edge_count: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeRec {
    pub subject: u32,
    pub object: u32,
    pub predicate: StrRef,
    pub passage: u32,
    pub confidence: f32,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone)]
pub(crate) struct AliasRec {
    pub alias: StrRef,
    pub target: StrRef,
    pub passage: u32,
    pub confidence: f32,
}

pub(crate) struct GraphTables {
    pub entities: Vec<EntityRec>,
    pub edges: Vec<EdgeRec>,
    by_object: Vec<u32>,
    /// Sorted by alias key.
    pub aliases: Vec<AliasRec>,
    /// Alias indices sorted by target key (built at open).
    pub aliases_by_target: Vec<u32>,
    lists: Vec<u8>,
    passage_count: u32,
}

fn bad(what: &str) -> ContextError {
    ContextError::corrupt(format!("invalid {what} record"))
}

fn count(bytes: &[u8], size: usize, what: &str) -> Result<usize> {
    if !bytes.len().is_multiple_of(size) {
        return Err(ContextError::corrupt(format!(
            "{what} section has a partial record"
        )));
    }
    Ok(bytes.len() / size)
}

impl GraphTables {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn parse(
        ents: &[u8],
        edges: &[u8],
        by_object: &[u8],
        aliases: &[u8],
        lists: Vec<u8>,
        strings: &str,
        passage_count: usize,
    ) -> Result<Self> {
        let passage_count = passage_count as u32;
        let n_edges = count(edges, EDGE_RECORD_LEN, "edge")?;
        let n_ents = count(ents, ENTITY_RECORD_LEN, "entity")?;
        let mut entities: Vec<EntityRec> = Vec::with_capacity(n_ents);
        for i in 0..n_ents {
            let mut d = Dec::at(ents, i * ENTITY_RECORD_LEN, "entity");
            let key = d.str_ref()?;
            let display = d.str_ref()?;
            check_str(strings, key)?;
            check_str(strings, display)?;
            EntityKind::from_code(d.u8()?).ok_or_else(|| bad("entity"))?;
            d.skip(3)?;
            let rec = EntityRec {
                key,
                display,
                defs: (d.u32()?, d.u32()?),
                mentions: (d.u32()?, d.u32()?),
                edge_first: d.u32()?,
                edge_count: d.u32()?,
            };
            let ordered = entities.last().is_none_or(|p| {
                strings[p.key.range()].as_bytes() < strings[key.range()].as_bytes()
            });
            let lists_ok = [rec.defs, rec.mentions]
                .iter()
                .all(|&(off, _)| (off as usize) <= lists.len());
            let edges_ok = u64::from(rec.edge_first) + u64::from(rec.edge_count) <= n_edges as u64;
            if key.len == 0 || !ordered || !lists_ok || !edges_ok {
                return Err(bad("entity"));
            }
            entities.push(rec);
        }

        let mut edge_recs = Vec::with_capacity(n_edges);
        for i in 0..n_edges {
            let mut d = Dec::at(edges, i * EDGE_RECORD_LEN, "edge");
            let rec = EdgeRec {
                subject: d.u32()?,
                object: d.u32()?,
                predicate: d.str_ref()?,
                passage: d.u32()?,
                confidence: conf_from_u16(d.u16()?),
                kind: EdgeKind::from_code(d.u8()?).ok_or_else(|| bad("edge"))?,
            };
            check_str(strings, rec.predicate)?;
            let ok = (rec.subject as usize) < n_ents
                && (rec.object as usize) < n_ents
                && rec.passage < passage_count;
            if !ok {
                return Err(bad("edge"));
            }
            edge_recs.push(rec);
        }
        for (i, e) in entities.iter().enumerate() {
            let run = &edge_recs[e.edge_first as usize..(e.edge_first + e.edge_count) as usize];
            if run.iter().any(|x| x.subject as usize != i) {
                return Err(bad("entity edge range"));
            }
        }

        if by_object.len() != n_edges * 4 {
            return Err(bad("reverse edge index"));
        }
        let mut order = Vec::with_capacity(n_edges);
        let mut d = Dec::new(by_object, "reverse edge index");
        for _ in 0..n_edges {
            let i = d.u32()?;
            let e = edge_recs
                .get(i as usize)
                .ok_or_else(|| bad("reverse edge index"))?;
            let sorted = order
                .last()
                .is_none_or(|&p: &u32| edge_recs[p as usize].object <= e.object);
            if !sorted {
                return Err(bad("reverse edge index"));
            }
            order.push(i);
        }

        let n_aliases = count(aliases, ALIAS_RECORD_LEN, "alias")?;
        let mut alias_recs: Vec<AliasRec> = Vec::with_capacity(n_aliases);
        for i in 0..n_aliases {
            let mut d = Dec::at(aliases, i * ALIAS_RECORD_LEN, "alias");
            let rec = AliasRec {
                alias: d.str_ref()?,
                target: d.str_ref()?,
                passage: d.u32()?,
                confidence: conf_from_u16(d.u16()?),
            };
            check_str(strings, rec.alias)?;
            check_str(strings, rec.target)?;
            let ordered = alias_recs.last().is_none_or(|p| {
                strings[p.alias.range()].as_bytes() <= strings[rec.alias.range()].as_bytes()
            });
            if !ordered || rec.passage >= passage_count {
                return Err(bad("alias"));
            }
            alias_recs.push(rec);
        }

        let mut aliases_by_target: Vec<u32> = (0..alias_recs.len() as u32).collect();
        aliases_by_target.sort_by(|&a, &b| {
            let ta = &strings[alias_recs[a as usize].target.range()];
            let tb = &strings[alias_recs[b as usize].target.range()];
            ta.as_bytes().cmp(tb.as_bytes())
        });

        Ok(GraphTables {
            entities,
            edges: edge_recs,
            by_object: order,
            aliases: alias_recs,
            aliases_by_target,
            lists,
            passage_count,
        })
    }

    /// Passages defining entity `e`.
    pub(crate) fn defs(&self, e: usize) -> Result<Vec<u32>> {
        let (off, n) = self.entities[e].defs;
        get_delta_list(&self.lists, off as usize, n as usize, self.passage_count)
    }

    /// Passages mentioning entity `e`.
    pub(crate) fn mentions(&self, e: usize) -> Result<Vec<u32>> {
        let (off, n) = self.entities[e].mentions;
        get_delta_list(&self.lists, off as usize, n as usize, self.passage_count)
    }

    /// Edges whose subject is entity `e`.
    pub(crate) fn out_edges(&self, e: usize) -> &[EdgeRec] {
        let r = &self.entities[e];
        &self.edges[r.edge_first as usize..(r.edge_first + r.edge_count) as usize]
    }

    /// Edges whose object is entity `e`.
    pub(crate) fn in_edges(&self, e: usize) -> impl Iterator<Item = &EdgeRec> {
        let e = e as u32;
        let lo = self
            .by_object
            .partition_point(|&i| self.edges[i as usize].object < e);
        let hi = self
            .by_object
            .partition_point(|&i| self.edges[i as usize].object <= e);
        self.by_object[lo..hi]
            .iter()
            .map(move |&i| &self.edges[i as usize])
    }
}
