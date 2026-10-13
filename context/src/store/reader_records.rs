//! Fixed-size record tables of a segment (documents, sections, passages,
//! terms), parsed and cross-checked at open.

use super::format::*;
use super::writer::role_from_code;
use crate::error::{ContextError, Result};
use crate::structure::Lang;
use crate::types::{DocKind, PassageKind, Role, SymbolKind, Trust};

#[derive(Debug, Clone)]
pub(crate) struct DocRec {
    pub key: StrRef,
    pub title: StrRef,
    pub path: StrRef,
    pub scope: StrRef,
    pub group: StrRef,
    pub kind: DocKind,
    pub trust: Trust,
    pub lang: Lang,
    pub role: Option<Role>,
    pub text_off: u64,
    pub text_len: u64,
    pub timestamp: i64,
    pub seq: u64,
    pub first_passage: u32,
    pub passage_count: u32,
    pub first_section: u32,
    pub section_count: u32,
    pub source_hash: [u8; 32],
    pub text_hash: [u8; 32],
}

#[derive(Debug, Clone)]
pub(crate) struct SectionRec {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct PassageRec {
    pub doc: u32,
    pub section: Option<u32>,
    pub kind: PassageKind,
    pub breadcrumb: StrRef,
    pub symbol: StrRef,
    pub start: u32,
    pub end: u32,
    pub line_start: u32,
    pub line_end: u32,
    pub length: u32,
    pub anchor_offset: u32,
    pub anchor_len: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct TermRec {
    pub term: StrRef,
    pub df: u32,
    pub post_len: u32,
    pub post_off: u64,
}

/// A string reference must land on character boundaries inside the table.
pub(crate) fn check_str(strings: &str, r: StrRef) -> Result<()> {
    strings
        .get(r.range())
        .map(|_| ())
        .ok_or_else(|| ContextError::corrupt("string reference out of bounds"))
}

fn records(bytes: &[u8], size: usize, what: &str) -> Result<usize> {
    if !bytes.len().is_multiple_of(size) {
        return Err(ContextError::corrupt(format!(
            "{what} section has a partial record"
        )));
    }
    Ok(bytes.len() / size)
}

fn bad(what: &str) -> ContextError {
    ContextError::corrupt(format!("invalid {what} record"))
}

pub(crate) fn parse_docs(bytes: &[u8], strings: &str, text_len: u64) -> Result<Vec<DocRec>> {
    let n = records(bytes, DOC_RECORD_LEN, "document")?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut d = Dec::at(bytes, i * DOC_RECORD_LEN, "document");
        let refs = [
            d.str_ref()?,
            d.str_ref()?,
            d.str_ref()?,
            d.str_ref()?,
            d.str_ref()?,
        ];
        for r in refs {
            check_str(strings, r)?;
        }
        let kind = DocKind::from_code(d.u8()?).ok_or_else(|| bad("document"))?;
        let trust = Trust::from_code(d.u8()?).ok_or_else(|| bad("document"))?;
        let lang = Lang::from_code(d.u8()?).ok_or_else(|| bad("document"))?;
        let role_code = d.u8()?;
        let role = match role_code {
            0 => None,
            c => Some(role_from_code(c).ok_or_else(|| bad("document"))?),
        };
        d.skip(4)?;
        let rec = DocRec {
            key: refs[0],
            title: refs[1],
            path: refs[2],
            scope: refs[3],
            group: refs[4],
            kind,
            trust,
            lang,
            role,
            text_off: d.u64()?,
            text_len: d.u64()?,
            timestamp: d.i64()?,
            seq: d.u64()?,
            first_passage: d.u32()?,
            passage_count: d.u32()?,
            first_section: d.u32()?,
            section_count: d.u32()?,
            source_hash: d.array()?,
            text_hash: d.array()?,
        };
        let text_end = rec.text_off.checked_add(rec.text_len);
        if rec.key.len == 0 || text_end.is_none_or(|e| e > text_len) {
            return Err(bad("document"));
        }
        out.push(rec);
    }
    Ok(out)
}

pub(crate) fn parse_sections(
    bytes: &[u8],
    strings: &str,
    docs: &[DocRec],
) -> Result<Vec<SectionRec>> {
    let n = records(bytes, SECTION_RECORD_LEN, "section")?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut d = Dec::at(bytes, i * SECTION_RECORD_LEN, "section");
        let title = d.str_ref()?;
        check_str(strings, title)?;
        let doc = d.u32()?;
        let parent = d.u32()?;
        d.skip(8)?;
        let (start, end) = (d.u64()?, d.u64()?);
        let doc_len = docs
            .get(doc as usize)
            .ok_or_else(|| bad("section"))?
            .text_len;
        let parent_ok = parent == NONE_U32 || (parent as usize) < i;
        if !parent_ok || start > end || end > doc_len {
            return Err(bad("section"));
        }
        out.push(SectionRec { start, end });
    }
    Ok(out)
}

pub(crate) fn parse_passages(
    bytes: &[u8],
    strings: &str,
    docs: &[DocRec],
    section_count: usize,
) -> Result<Vec<PassageRec>> {
    let n = records(bytes, PASSAGE_RECORD_LEN, "passage")?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut d = Dec::at(bytes, i * PASSAGE_RECORD_LEN, "passage");
        let doc = d.u32()?;
        let section = d.u32()?;
        let kind = PassageKind::from_code(d.u8()?).ok_or_else(|| bad("passage"))?;
        let sym_code = d.u8()?;
        if sym_code != 0 && SymbolKind::from_code(sym_code).is_none() {
            return Err(bad("passage"));
        }
        d.skip(2)?;
        let breadcrumb = d.str_ref()?;
        let symbol = d.str_ref()?;
        check_str(strings, breadcrumb)?;
        check_str(strings, symbol)?;
        let rec = PassageRec {
            doc,
            section: (section != NONE_U32).then_some(section),
            kind,
            breadcrumb,
            symbol,
            start: d.u32()?,
            end: d.u32()?,
            line_start: d.u32()?,
            line_end: d.u32()?,
            length: d.u32()?,
            anchor_offset: {
                d.skip(4)?; // token estimate (reserved for packers)
                d.u32()?
            },
            anchor_len: d.u32()?,
        };
        let doc_len = docs
            .get(doc as usize)
            .ok_or_else(|| bad("passage"))?
            .text_len;
        let section_ok = section == NONE_U32 || (section as usize) < section_count;
        let ok = section_ok
            && rec.start < rec.end
            && u64::from(rec.end) <= doc_len
            && rec.line_start >= 1
            && rec.line_start <= rec.line_end;
        if !ok {
            return Err(bad("passage"));
        }
        out.push(rec);
    }
    Ok(out)
}

pub(crate) fn parse_terms(
    bytes: &[u8],
    strings: &str,
    postings_len: u64,
    passage_count: usize,
) -> Result<Vec<TermRec>> {
    let n = records(bytes, TERM_ENTRY_LEN, "term")?;
    let mut out: Vec<TermRec> = Vec::with_capacity(n);
    for i in 0..n {
        let mut d = Dec::at(bytes, i * TERM_ENTRY_LEN, "term");
        let rec = TermRec {
            term: d.str_ref()?,
            df: d.u32()?,
            post_len: d.u32()?,
            post_off: d.u64()?,
        };
        check_str(strings, rec.term)?;
        let in_bounds = rec
            .post_off
            .checked_add(u64::from(rec.post_len))
            .is_some_and(|e| e <= postings_len);
        let df_ok = rec.df >= 1 && rec.df as usize <= passage_count;
        let ordered = out.last().is_none_or(|prev| {
            strings[prev.term.range()].as_bytes() < strings[rec.term.range()].as_bytes()
        });
        if rec.term.len == 0 || !in_bounds || !df_ok || !ordered {
            return Err(bad("term"));
        }
        out.push(rec);
    }
    Ok(out)
}
