//! Segment writer: compiled documents to `.ctxs` bytes.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh64::xxh64;

use super::format::*;
use super::writer_graph::encode_graph;
use crate::compile::{CompiledDoc, COMPILER_VERSION};
use crate::error::{ContextError, Result};
use crate::graph::GraphMode;

/// Segment-level metadata stored as JSON in the META section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SegmentMeta {
    pub created_unix: i64,
    pub docs: u32,
    pub sections: u32,
    pub passages: u32,
    pub terms: u32,
    /// Sum of passage lengths (BM25 document lengths).
    pub total_length: u64,
    pub graph: GraphMode,
    pub entities: u32,
    pub edges: u32,
    pub aliases: u32,
    /// Crate version that wrote the segment.
    pub writer: String,
    /// [`COMPILER_VERSION`] that compiled the segment's documents (0 when
    /// unknown).
    #[serde(default)]
    pub compiler: u32,
}

/// Deduplicating builder for the STRINGS section.
#[derive(Default)]
pub(crate) struct Strings {
    pub blob: Vec<u8>,
    map: HashMap<String, StrRef>,
}

impl Strings {
    pub(crate) fn add(&mut self, s: &str) -> Result<StrRef> {
        if s.is_empty() {
            return Ok(StrRef::default());
        }
        if let Some(r) = self.map.get(s) {
            return Ok(*r);
        }
        let off = u32::try_from(self.blob.len())
            .map_err(|_| ContextError::invalid("segment string table exceeds 4 GiB"))?;
        let len = u32::try_from(s.len()).map_err(|_| ContextError::invalid("string too long"))?;
        self.blob.extend_from_slice(s.as_bytes());
        let r = StrRef { off, len };
        self.map.insert(s.to_string(), r);
        Ok(r)
    }
}

/// Encode `docs` into a complete segment file image.
pub(crate) fn write_segment(
    docs: &[CompiledDoc],
    graph: GraphMode,
    created_unix: i64,
) -> Result<(Vec<u8>, SegmentMeta)> {
    let mut strings = Strings::default();
    let mut text = Vec::new();
    let mut docs_enc = Enc::default();
    let mut sections_enc = Enc::default();
    let mut passages_enc = Enc::default();
    let mut terms: BTreeMap<&str, Vec<(u32, u32)>> = BTreeMap::new();
    let mut pid: u32 = 0;
    let mut sid: u32 = 0;
    let mut total_length: u64 = 0;

    for (di, d) in docs.iter().enumerate() {
        let di = to_u32(di, "documents")?;
        let text_off = text.len() as u64;
        text.extend_from_slice(d.text.as_bytes());
        let first_section = sid;
        for s in &d.sections {
            sections_enc.str_ref(strings.add(&s.title)?);
            sections_enc.u32(di);
            sections_enc.u32(s.parent.map_or(NONE_U32, |p| first_section + p));
            sections_enc.u8(s.depth);
            sections_enc.zeros(7);
            sections_enc.u64(s.start as u64);
            sections_enc.u64(s.end as u64);
            sid = sid.checked_add(1).ok_or_else(|| overflow("sections"))?;
        }
        let first_passage = pid;
        for p in &d.passages {
            let span = &p.span;
            passages_enc.u32(di);
            passages_enc.u32(span.section.map_or(NONE_U32, |s| first_section + s));
            passages_enc.u8(span.kind.code());
            passages_enc.u8(span.symbol.as_ref().map_or(0, |s| s.kind.code()));
            passages_enc.u16(0);
            passages_enc.str_ref(strings.add(&span.breadcrumb)?);
            passages_enc
                .str_ref(strings.add(span.symbol.as_ref().map_or("", |s| s.name.as_str()))?);
            passages_enc.u32(to_u32(span.start, "passage offset")?);
            passages_enc.u32(to_u32(span.end, "passage offset")?);
            passages_enc.u32(span.line_start);
            passages_enc.u32(span.line_end);
            passages_enc.u32(p.length);
            passages_enc.u32(p.tokens);
            passages_enc.u32(span.anchor_offset);
            passages_enc.u32(span.anchor_len);
            passages_enc.u32(0);
            for (term, tf) in &p.terms {
                terms.entry(term.as_str()).or_default().push((pid, *tf));
            }
            total_length += u64::from(p.length);
            pid = pid.checked_add(1).ok_or_else(|| overflow("passages"))?;
        }
        let m = &d.meta;
        docs_enc.str_ref(strings.add(m.key.as_str())?);
        docs_enc.str_ref(strings.add(&m.title)?);
        docs_enc.str_ref(strings.add(m.path.as_deref().unwrap_or(""))?);
        docs_enc.str_ref(strings.add(m.scope.as_str())?);
        docs_enc.str_ref(strings.add(m.group.as_deref().unwrap_or(""))?);
        docs_enc.u8(m.kind.code());
        docs_enc.u8(m.trust.code());
        docs_enc.u8(m.lang.code());
        docs_enc.u8(m.role.map_or(0, role_code));
        docs_enc.u32(0);
        docs_enc.u64(text_off);
        docs_enc.u64(d.text.len() as u64);
        docs_enc.i64(m.timestamp);
        docs_enc.u64(m.seq);
        docs_enc.u32(first_passage);
        docs_enc.u32(to_u32(d.passages.len(), "passages")?);
        docs_enc.u32(first_section);
        docs_enc.u32(to_u32(d.sections.len(), "sections")?);
        docs_enc.bytes(&m.source_hash);
        docs_enc.bytes(&m.text_hash);
    }

    let mut terms_enc = Enc::default();
    let mut postings = Vec::new();
    for (term, posts) in &terms {
        let post_off = postings.len() as u64;
        let mut prev = 0u32;
        for (i, &(p, tf)) in posts.iter().enumerate() {
            put_varint(&mut postings, u64::from(if i == 0 { p } else { p - prev }));
            put_varint(&mut postings, u64::from(tf));
            prev = p;
        }
        terms_enc.str_ref(strings.add(term)?);
        terms_enc.u32(to_u32(posts.len(), "postings")?);
        terms_enc.u32(to_u32(postings.len() - post_off as usize, "posting bytes")?);
        terms_enc.u64(post_off);
    }

    let graph_sections = if graph == GraphMode::Off {
        None
    } else {
        Some(encode_graph(docs, &mut strings)?)
    };

    let meta = SegmentMeta {
        created_unix,
        docs: to_u32(docs.len(), "documents")?,
        sections: sid,
        passages: pid,
        terms: to_u32(terms.len(), "terms")?,
        total_length,
        graph,
        entities: graph_sections.as_ref().map_or(0, |g| g.entities),
        edges: graph_sections.as_ref().map_or(0, |g| g.edges),
        aliases: graph_sections.as_ref().map_or(0, |g| g.aliases),
        writer: env!("CARGO_PKG_VERSION").to_string(),
        compiler: COMPILER_VERSION,
    };
    let meta_json = serde_json::to_vec(&meta).map_err(|e| ContextError::invalid(e.to_string()))?;

    let mut sections: Vec<(SectionKind, Vec<u8>)> = vec![
        (SectionKind::Meta, meta_json),
        (SectionKind::Strings, strings.blob),
        (SectionKind::Docs, docs_enc.buf),
        (SectionKind::Sections, sections_enc.buf),
        (SectionKind::Passages, passages_enc.buf),
        (SectionKind::Text, text),
        (SectionKind::Terms, terms_enc.buf),
        (SectionKind::Postings, postings),
    ];
    if let Some(g) = graph_sections {
        sections.push((SectionKind::Entities, g.entities_buf));
        sections.push((SectionKind::Edges, g.edges_buf));
        sections.push((SectionKind::EdgesByObject, g.by_object_buf));
        sections.push((SectionKind::Aliases, g.aliases_buf));
        sections.push((SectionKind::GraphLists, g.lists_buf));
    }
    Ok((assemble(sections, created_unix), meta))
}

pub(super) fn assemble(sections: Vec<(SectionKind, Vec<u8>)>, created_unix: i64) -> Vec<u8> {
    let raw = sections
        .into_iter()
        .map(|(kind, bytes)| (kind as u32, 0, bytes))
        .collect();
    assemble_raw(raw, created_unix)
}

/// Assemble sections given as `(kind, flags, bytes)`.
pub(super) fn assemble_raw(sections: Vec<(u32, u32, Vec<u8>)>, created_unix: i64) -> Vec<u8> {
    let mut out = Enc::default();
    out.zeros(HEADER_LEN);
    let mut toc = Enc::default();
    let has_graph = sections
        .iter()
        .any(|(k, _, _)| *k == SectionKind::Entities as u32);
    let count = sections.len() as u32;
    for (kind, flags, bytes) in sections {
        out.align(8);
        let off = out.buf.len() as u64;
        toc.u32(kind);
        toc.u32(flags);
        toc.u64(off);
        toc.u64(bytes.len() as u64);
        toc.u64(xxh64(&bytes, 0));
        out.bytes(&bytes);
    }
    out.align(8);
    let toc_off = out.buf.len() as u64;
    let toc_len = toc.buf.len() as u64;
    let toc_sum = xxh64(&toc.buf, 0);
    out.bytes(&toc.buf);
    out.u64(toc_off);
    out.u64(toc_len);
    out.u64(toc_sum);
    out.bytes(FOOTER_MAGIC);

    let mut header = Enc::default();
    header.bytes(MAGIC);
    header.u32(FORMAT_VERSION);
    header.u32(u32::from(has_graph));
    header.u32(count);
    header.u32(0);
    header.i64(created_unix);
    header.bytes(uuid::Uuid::new_v4().as_bytes());
    header.zeros(HEADER_LEN - header.buf.len());
    out.buf[..HEADER_LEN].copy_from_slice(&header.buf);
    out.buf
}

pub(crate) fn role_code(role: crate::types::Role) -> u8 {
    use crate::types::Role;
    match role {
        Role::System => 1,
        Role::User => 2,
        Role::Assistant => 3,
        Role::Tool => 4,
    }
}

pub(crate) fn role_from_code(code: u8) -> Option<crate::types::Role> {
    use crate::types::Role;
    Some(match code {
        1 => Role::System,
        2 => Role::User,
        3 => Role::Assistant,
        4 => Role::Tool,
        _ => return None,
    })
}

pub(crate) fn to_u32(v: usize, what: &str) -> Result<u32> {
    u32::try_from(v).map_err(|_| overflow(what))
}

fn overflow(what: &str) -> ContextError {
    ContextError::invalid(format!("too many {what} for one segment"))
}
