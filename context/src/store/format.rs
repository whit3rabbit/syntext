//! Segment file format (`.ctxs`): constants, record sizes, and little-endian
//! encoding helpers.
//!
//! ```text
//! +----------------------------+
//! | Header (64 bytes)          |  magic "SXCTXSEG", version, section count,
//! |                            |  creation time, segment uuid
//! +----------------------------+
//! | Sections, 8-byte aligned   |  META, STRINGS, DOCS, SECTIONS, PASSAGES,
//! |                            |  TEXT, TERMS, POSTINGS [, graph sections]
//! +----------------------------+
//! | TOC (32 bytes per section) |  kind, flags, offset, length, xxh64
//! +----------------------------+
//! | Footer (32 bytes)          |  TOC offset/length/checksum, "SXCTXEND"
//! +----------------------------+
//! ```
//!
//! Segments are immutable: written once to a temporary name, fsynced, and
//! renamed into place. Every section has its own checksum, so a reader can
//! verify the small metadata sections on open and defer the large TEXT and
//! POSTINGS checks to an explicit `verify`. All integers are little-endian.
//! Strings are `(offset, length)` references into the STRINGS section.
//!
//! A section whose TOC flags carry [`SECTION_OPTIONAL`] may be skipped by a
//! reader that does not know its kind, so later versions can add sections
//! (vectors, for example) that older readers ignore. An unknown section
//! without the flag fails the open.

use crate::error::{ContextError, Result};

pub(crate) const MAGIC: &[u8; 8] = b"SXCTXSEG";
pub(crate) const FOOTER_MAGIC: &[u8; 8] = b"SXCTXEND";
/// Current (and only) segment format version.
pub(crate) const FORMAT_VERSION: u32 = 1;

pub(crate) const HEADER_LEN: usize = 64;
pub(crate) const FOOTER_LEN: usize = 32;
pub(crate) const TOC_ENTRY_LEN: usize = 32;
/// Upper bound on TOC entries a reader accepts.
pub(crate) const MAX_SECTIONS: usize = 64;

pub(crate) const DOC_RECORD_LEN: usize = 160;
pub(crate) const SECTION_RECORD_LEN: usize = 40;
pub(crate) const PASSAGE_RECORD_LEN: usize = 64;
pub(crate) const TERM_ENTRY_LEN: usize = 24;
pub(crate) const ENTITY_RECORD_LEN: usize = 48;
pub(crate) const EDGE_RECORD_LEN: usize = 24;
pub(crate) const ALIAS_RECORD_LEN: usize = 24;

/// Sentinel for "no index" in u32 reference fields.
pub(crate) const NONE_U32: u32 = u32::MAX;

/// TOC flag: readers that do not know this section's kind skip it.
pub(crate) const SECTION_OPTIONAL: u32 = 1;

/// Section identifiers stored in the TOC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SectionKind {
    Meta = 1,
    Strings = 2,
    Docs = 3,
    Sections = 4,
    Passages = 5,
    Text = 6,
    Terms = 7,
    Postings = 8,
    Entities = 9,
    Edges = 10,
    EdgesByObject = 11,
    Aliases = 12,
    GraphLists = 13,
}

impl SectionKind {
    pub(crate) fn from_u32(v: u32) -> Option<Self> {
        use SectionKind::*;
        Some(match v {
            1 => Meta,
            2 => Strings,
            3 => Docs,
            4 => Sections,
            5 => Passages,
            6 => Text,
            7 => Terms,
            8 => Postings,
            9 => Entities,
            10 => Edges,
            11 => EdgesByObject,
            12 => Aliases,
            13 => GraphLists,
            _ => return None,
        })
    }
}

/// Reference to a string in the STRINGS section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct StrRef {
    pub off: u32,
    pub len: u32,
}

impl StrRef {
    pub(crate) fn range(self) -> std::ops::Range<usize> {
        self.off as usize..self.off as usize + self.len as usize
    }
}

/// Little-endian byte writer.
#[derive(Default)]
pub(crate) struct Enc {
    pub buf: Vec<u8>,
}

impl Enc {
    pub(crate) fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub(crate) fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    pub(crate) fn str_ref(&mut self, r: StrRef) {
        self.u32(r.off);
        self.u32(r.len);
    }
    pub(crate) fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }
    /// Pad with zeros to a multiple of `align`.
    pub(crate) fn align(&mut self, align: usize) {
        let rem = self.buf.len() % align;
        if rem != 0 {
            self.zeros(align - rem);
        }
    }
}

/// Bounds-checked little-endian reader. Every read past the end is a
/// [`ContextError::Corrupt`], never a panic.
pub(crate) struct Dec<'a> {
    b: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Dec<'a> {
    pub(crate) fn new(b: &'a [u8], what: &'static str) -> Self {
        Dec { b, pos: 0, what }
    }

    pub(crate) fn at(b: &'a [u8], pos: usize, what: &'static str) -> Self {
        Dec { b, pos, what }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| ContextError::corrupt(format!("truncated {}", self.what)))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub(crate) fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }
    pub(crate) fn str_ref(&mut self) -> Result<StrRef> {
        Ok(StrRef {
            off: self.u32()?,
            len: self.u32()?,
        })
    }
    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
}

/// Append `v` as an unsigned LEB128 varint.
pub(crate) fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

/// Decode a varint at `*pos`, advancing it. Rejects overlong encodings
/// (more than 10 bytes) and truncated input.
pub(crate) fn get_varint(b: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *b
            .get(*pos)
            .ok_or_else(|| ContextError::corrupt("truncated varint"))?;
        *pos += 1;
        if shift == 63 && byte > 1 {
            return Err(ContextError::corrupt("varint overflow"));
        }
        v |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err(ContextError::corrupt("overlong varint"));
        }
    }
}

/// Encode a sorted list of ids as varint deltas.
pub(crate) fn put_delta_list(buf: &mut Vec<u8>, ids: &[u32]) {
    let mut prev = 0u32;
    for (i, &id) in ids.iter().enumerate() {
        let delta = if i == 0 { id } else { id - prev };
        put_varint(buf, u64::from(delta));
        prev = id;
    }
}

/// Decode `count` varint deltas starting at `pos`, each id `< limit` and
/// strictly increasing.
pub(crate) fn get_delta_list(
    b: &[u8],
    mut pos: usize,
    count: usize,
    limit: u32,
) -> Result<Vec<u32>> {
    let mut out = Vec::with_capacity(count.min(1 << 16));
    let mut prev: u64 = 0;
    for i in 0..count {
        let delta = get_varint(b, &mut pos)?;
        let id = if i == 0 { delta } else { prev + delta };
        if (i > 0 && delta == 0) || id >= u64::from(limit) {
            return Err(ContextError::corrupt("invalid id list"));
        }
        out.push(id as u32);
        prev = id;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip_and_reject_garbage() {
        let mut buf = Vec::new();
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            buf.clear();
            put_varint(&mut buf, v);
            let mut pos = 0;
            assert_eq!(get_varint(&buf, &mut pos).unwrap(), v);
            assert_eq!(pos, buf.len());
        }
        let mut pos = 0;
        assert!(get_varint(&[0x80, 0x80], &mut pos).is_err(), "truncated");
        let mut pos = 0;
        assert!(get_varint(&[0xFF; 11], &mut pos).is_err(), "overlong");
        let mut pos = 0;
        assert!(get_varint(
            &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02],
            &mut pos
        )
        .is_err());
    }

    #[test]
    fn delta_lists_validate_order_and_bounds() {
        let mut buf = Vec::new();
        put_delta_list(&mut buf, &[3, 7, 8, 100]);
        assert_eq!(get_delta_list(&buf, 0, 4, 101).unwrap(), [3, 7, 8, 100]);
        assert!(get_delta_list(&buf, 0, 4, 100).is_err(), "id out of range");
        let mut dup = Vec::new();
        put_varint(&mut dup, 5);
        put_varint(&mut dup, 0);
        assert!(get_delta_list(&dup, 0, 2, 10).is_err(), "duplicate id");
    }

    #[test]
    fn decoder_reports_truncation() {
        let mut d = Dec::new(&[1, 2, 3], "test");
        assert_eq!(d.u16().unwrap(), 0x0201);
        assert!(d.u32().is_err());
        let mut d = Dec::at(&[0; 4], 3, "test");
        assert!(d.u16().is_err());
    }

    #[test]
    fn encoder_aligns() {
        let mut e = Enc::default();
        e.u8(1);
        e.align(8);
        assert_eq!(e.buf.len(), 8);
        e.align(8);
        assert_eq!(e.buf.len(), 8);
    }
}
