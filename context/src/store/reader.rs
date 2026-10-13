//! Segment reader: validates a `.ctxs` image and serves its tables.
//!
//! A segment file is treated as untrusted input. Opening checks magic,
//! version, TOC bounds and checksums, every metadata section's checksum, and
//! every cross-reference (string ranges, doc/section/passage indices, term
//! order, posting ranges). After a successful open, accessors cannot panic
//! on segment contents; a bad posting list or text range surfaces as
//! [`ContextError::Corrupt`].

use std::path::Path;
use std::sync::Arc;

use xxhash_rust::xxh64::xxh64;

use super::backing::Backing;
use super::format::*;
use super::reader_graph::{AliasRec, GraphTables};
use super::reader_records::{parse_docs, parse_passages, parse_sections, parse_terms};
pub(crate) use super::reader_records::{DocRec, PassageRec, SectionRec, TermRec};
use super::writer::SegmentMeta;
use crate::error::{ContextError, Result};

/// Location and checksum of one section.
#[derive(Debug, Clone, Copy)]
struct SectionLoc {
    kind: SectionKind,
    off: u64,
    len: u64,
    checksum: u64,
}

pub(crate) struct SegmentReader {
    backing: Backing,
    pub meta: SegmentMeta,
    strings: String,
    pub docs: Vec<DocRec>,
    pub sections: Vec<SectionRec>,
    pub passages: Vec<PassageRec>,
    terms: Vec<TermRec>,
    text: SectionLoc,
    postings: SectionLoc,
    pub graph: Option<GraphTables>,
}

impl std::fmt::Debug for SegmentReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SegmentReader")
            .field("docs", &self.docs.len())
            .field("passages", &self.passages.len())
            .field("terms", &self.terms.len())
            .finish()
    }
}

impl SegmentReader {
    /// Open a segment file. `verify_bulk` also checksums TEXT and POSTINGS.
    pub(crate) fn open_file(path: &Path, verify_bulk: bool) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::parse(Backing::file(file)?, verify_bulk)
    }

    /// Open an in-memory segment image (always fully verified).
    pub(crate) fn from_bytes(bytes: Arc<[u8]>) -> Result<Self> {
        Self::parse(Backing::Memory(bytes), true)
    }

    fn parse(backing: Backing, verify_bulk: bool) -> Result<Self> {
        let len = backing.len();
        if len < (HEADER_LEN + FOOTER_LEN) as u64 {
            return Err(ContextError::corrupt("segment too short"));
        }
        let header = backing.read_at(0, HEADER_LEN)?;
        let mut h = Dec::new(&header, "header");
        if &h.array::<8>()? != MAGIC {
            return Err(ContextError::corrupt("bad segment magic"));
        }
        let version = h.u32()?;
        if version != FORMAT_VERSION {
            return Err(ContextError::UnsupportedVersion {
                found: version,
                supported: FORMAT_VERSION,
            });
        }
        let _flags = h.u32()?;
        let section_count = h.u32()? as usize;

        let footer = backing.read_at(len - FOOTER_LEN as u64, FOOTER_LEN)?;
        let mut f = Dec::new(&footer, "footer");
        let toc_off = f.u64()?;
        let toc_len = f.u64()?;
        let toc_sum = f.u64()?;
        if &f.array::<8>()? != FOOTER_MAGIC {
            return Err(ContextError::corrupt("bad segment footer magic"));
        }
        let toc_ok = toc_len == (section_count * TOC_ENTRY_LEN) as u64
            && section_count <= MAX_SECTIONS
            && toc_off >= HEADER_LEN as u64
            && toc_off.checked_add(toc_len) == Some(len - FOOTER_LEN as u64);
        if !toc_ok {
            return Err(ContextError::corrupt("bad segment table of contents"));
        }
        let toc = backing.read_at(toc_off, toc_len as usize)?;
        if xxh64(&toc, 0) != toc_sum {
            return Err(ContextError::corrupt("table of contents checksum mismatch"));
        }
        let locs = parse_toc(&toc, section_count, toc_off)?;
        let find = |k: SectionKind| locs.iter().find(|l| l.kind == k).copied();
        let need = |k: SectionKind| {
            find(k).ok_or_else(|| ContextError::corrupt(format!("missing section {k:?}")))
        };
        let load = |loc: SectionLoc| -> Result<Vec<u8>> {
            let bytes = backing.read_at(loc.off, loc.len as usize)?.into_owned();
            if xxh64(&bytes, 0) != loc.checksum {
                return Err(ContextError::corrupt(format!(
                    "{:?} checksum mismatch",
                    loc.kind
                )));
            }
            Ok(bytes)
        };

        let meta: SegmentMeta = serde_json::from_slice(&load(need(SectionKind::Meta)?)?)
            .map_err(|e| ContextError::corrupt(format!("segment meta: {e}")))?;
        let strings = String::from_utf8(load(need(SectionKind::Strings)?)?)
            .map_err(|_| ContextError::corrupt("string table is not UTF-8"))?;
        let text = need(SectionKind::Text)?;
        let postings = need(SectionKind::Postings)?;
        let docs = parse_docs(&load(need(SectionKind::Docs)?)?, &strings, text.len)?;
        let sections = parse_sections(&load(need(SectionKind::Sections)?)?, &strings, &docs)?;
        let passages = parse_passages(
            &load(need(SectionKind::Passages)?)?,
            &strings,
            &docs,
            sections.len(),
        )?;
        check_doc_ranges(&docs, &passages, sections.len())?;
        let terms = parse_terms(
            &load(need(SectionKind::Terms)?)?,
            &strings,
            postings.len,
            passages.len(),
        )?;

        let graph = match find(SectionKind::Entities) {
            None => None,
            Some(ents) => Some(GraphTables::parse(
                &load(ents)?,
                &load(need(SectionKind::Edges)?)?,
                &load(need(SectionKind::EdgesByObject)?)?,
                &load(need(SectionKind::Aliases)?)?,
                load(need(SectionKind::GraphLists)?)?,
                &strings,
                passages.len(),
            )?),
        };
        let reader = SegmentReader {
            backing,
            meta,
            strings,
            docs,
            sections,
            passages,
            terms,
            text,
            postings,
            graph,
        };
        if verify_bulk {
            reader.verify_bulk()?;
        }
        Ok(reader)
    }

    /// Size of the segment image in bytes.
    pub(crate) fn byte_len(&self) -> u64 {
        self.backing.len()
    }

    /// Checksum the TEXT and POSTINGS sections.
    pub(crate) fn verify_bulk(&self) -> Result<()> {
        for loc in [self.text, self.postings] {
            let bytes = self.backing.read_at(loc.off, loc.len as usize)?;
            if xxh64(&bytes, 0) != loc.checksum {
                return Err(ContextError::corrupt(format!(
                    "{:?} checksum mismatch",
                    loc.kind
                )));
            }
        }
        Ok(())
    }

    /// Resolve a string reference (validated at open; empty on mismatch).
    pub(crate) fn str(&self, r: StrRef) -> &str {
        self.strings.get(r.range()).unwrap_or("")
    }

    /// Index of `term` in the sorted dictionary.
    pub(crate) fn term_index(&self, term: &str) -> Option<usize> {
        self.terms
            .binary_search_by(|t| self.str(t.term).as_bytes().cmp(term.as_bytes()))
            .ok()
    }

    /// Document frequency of the term at `idx`.
    pub(crate) fn term_df(&self, idx: usize) -> u32 {
        self.terms[idx].df
    }

    /// Number of distinct terms.
    #[cfg(test)]
    pub(crate) fn term_count(&self) -> usize {
        self.terms.len()
    }

    /// Decode the posting list `(passage, weighted tf)` of the term at `idx`.
    pub(crate) fn postings(&self, idx: usize) -> Result<Vec<(u32, u32)>> {
        let t = &self.terms[idx];
        let bytes = self
            .backing
            .read_at(self.postings.off + t.post_off, t.post_len as usize)?;
        let limit = self.passages.len() as u64;
        let mut out = Vec::with_capacity(t.df as usize);
        let mut pos = 0;
        let mut prev: u64 = 0;
        for i in 0..t.df {
            let delta = get_varint(&bytes, &mut pos)?;
            let pid = if i == 0 { delta } else { prev + delta };
            let tf = get_varint(&bytes, &mut pos)?;
            if (i > 0 && delta == 0) || pid >= limit || tf > u64::from(u32::MAX) {
                return Err(ContextError::corrupt("invalid posting list"));
            }
            out.push((pid as u32, tf as u32));
            prev = pid;
        }
        Ok(out)
    }

    /// Bytes `[start, end)` of document `doc`'s text.
    pub(crate) fn text_range(&self, doc: u32, start: usize, end: usize) -> Result<String> {
        let d = self
            .docs
            .get(doc as usize)
            .ok_or_else(|| ContextError::corrupt("document index out of range"))?;
        if start > end || end as u64 > d.text_len {
            return Err(ContextError::corrupt("text range outside document"));
        }
        let bytes = self
            .backing
            .read_at(self.text.off + d.text_off + start as u64, end - start)?;
        String::from_utf8(bytes.into_owned())
            .map_err(|_| ContextError::corrupt("passage text is not UTF-8"))
    }

    /// Full text of document `doc`.
    pub(crate) fn doc_text(&self, doc: u32) -> Result<String> {
        let len = self
            .docs
            .get(doc as usize)
            .map_or(0, |d| d.text_len as usize);
        self.text_range(doc, 0, len)
    }

    /// Index of the entity with canonical `key`.
    pub(crate) fn entity_index(&self, key: &str) -> Option<usize> {
        let g = self.graph.as_ref()?;
        g.entities
            .binary_search_by(|e| self.str(e.key).as_bytes().cmp(key.as_bytes()))
            .ok()
    }

    /// Aliases whose alias key is `alias` (alias -> canonical target).
    pub(crate) fn aliases_from(&self, alias: &str) -> Vec<&AliasRec> {
        let Some(g) = &self.graph else {
            return Vec::new();
        };
        let lo = g
            .aliases
            .partition_point(|a| self.str(a.alias).as_bytes() < alias.as_bytes());
        g.aliases[lo..]
            .iter()
            .take_while(|a| self.str(a.alias) == alias)
            .collect()
    }

    /// Aliases whose target key is `target` (canonical -> its aliases).
    pub(crate) fn aliases_to(&self, target: &str) -> Vec<&AliasRec> {
        let Some(g) = &self.graph else {
            return Vec::new();
        };
        let key = |i: &u32| self.str(g.aliases[*i as usize].target);
        let lo = g
            .aliases_by_target
            .partition_point(|i| key(i).as_bytes() < target.as_bytes());
        g.aliases_by_target[lo..]
            .iter()
            .take_while(|i| key(i) == target)
            .map(|&i| &g.aliases[i as usize])
            .collect()
    }

    /// Text of passage `pid`.
    pub(crate) fn passage_text(&self, pid: u32) -> Result<String> {
        let p = self
            .passages
            .get(pid as usize)
            .ok_or_else(|| ContextError::corrupt("passage index out of range"))?;
        self.text_range(p.doc, p.start as usize, p.end as usize)
    }
}

fn parse_toc(toc: &[u8], count: usize, toc_off: u64) -> Result<Vec<SectionLoc>> {
    let mut d = Dec::new(toc, "toc");
    let mut locs: Vec<SectionLoc> = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = SectionKind::from_u32(d.u32()?)
            .ok_or_else(|| ContextError::corrupt("unknown section kind"))?;
        d.skip(4)?;
        let (off, len, checksum) = (d.u64()?, d.u64()?, d.u64()?);
        let in_bounds =
            off >= HEADER_LEN as u64 && off.checked_add(len).is_some_and(|e| e <= toc_off);
        if !in_bounds || locs.iter().any(|l| l.kind == kind) {
            return Err(ContextError::corrupt("bad section entry"));
        }
        locs.push(SectionLoc {
            kind,
            off,
            len,
            checksum,
        });
    }
    let mut sorted = locs.clone();
    sorted.sort_by_key(|l| l.off);
    if sorted.windows(2).any(|w| w[0].off + w[0].len > w[1].off) {
        return Err(ContextError::corrupt("overlapping sections"));
    }
    Ok(locs)
}

/// Every document's passages and sections must be the contiguous runs its
/// record claims, and nothing else.
fn check_doc_ranges(docs: &[DocRec], passages: &[PassageRec], section_count: usize) -> Result<()> {
    let mut next_p = 0u64;
    let mut next_s = 0u64;
    for (di, d) in docs.iter().enumerate() {
        let p_end = u64::from(d.first_passage) + u64::from(d.passage_count);
        let s_end = u64::from(d.first_section) + u64::from(d.section_count);
        if u64::from(d.first_passage) != next_p || u64::from(d.first_section) != next_s {
            return Err(ContextError::corrupt("document ranges are not contiguous"));
        }
        if p_end > passages.len() as u64 || s_end > section_count as u64 {
            return Err(ContextError::corrupt("document range out of bounds"));
        }
        let owned = &passages[d.first_passage as usize..p_end as usize];
        if owned.iter().any(|p| p.doc as usize != di) {
            return Err(ContextError::corrupt("passage owned by the wrong document"));
        }
        next_p = p_end;
        next_s = s_end;
    }
    if next_p != passages.len() as u64 || next_s != section_count as u64 {
        return Err(ContextError::corrupt("orphan passages or sections"));
    }
    Ok(())
}
