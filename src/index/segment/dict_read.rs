//! Dictionary binary-search and doc-table lookups for `MmapSegment`.
//!
//! Extracted from `mod.rs` to keep non-test line count under 400. Each
//! operation has two implementations dispatched on whether a backing file is
//! available:
//! - `*_mmap`: indexes directly into the mmap/heap slice (used for in-memory
//!   segments, i.e. `from_bytes` on WASM / tests, which have no file).
//! - `*_pread`: reads via positional reads (`pread`) against the open file,
//!   used by every native `open()`/`open_split()` segment. See
//!   [`MmapSegment::get_doc_pread`] for the security/availability rationale
//!   for preferring `pread` over the mmap slice at these two call sites.

#[cfg(feature = "memmap2")]
use super::reader;
use super::{DocEntry, MmapSegment, DICT_ENTRY_SIZE};
use crate::path_util::path_from_bytes;
use crate::posting::PostingList;

impl MmapSegment {
    /// Look up the posting list for a gram. Returns `None` if not present.
    pub fn lookup_gram(&self, gram_hash: u64) -> Option<PostingList> {
        self.check_len()?;
        let (abs_off, _) = self.dict_lookup(gram_hash)?;
        self.read_posting_list(abs_off)
    }

    /// Entry count for a gram (for cardinality-based intersection ordering).
    pub fn gram_cardinality(&self, gram_hash: u64) -> Option<u32> {
        self.check_len()?;
        Some(self.dict_lookup(gram_hash)?.1)
    }

    /// Return the `DocEntry` for a local doc_id (0-based within this segment).
    ///
    /// Dispatches to a positional-read (`pread`) path when a backing file is
    /// available (`_file.is_some()`, i.e. every native `open()`/`open_split()`
    /// segment), falling back to the mmap/heap slice for in-memory segments
    /// (`from_bytes`, used by WASM and some tests) which have no file to read
    /// from. See [`Self::get_doc_pread`] for the security rationale.
    pub fn get_doc(&self, doc_id: u32) -> Option<DocEntry> {
        self.check_len()?;
        if doc_id >= self.doc_count {
            return None;
        }
        #[cfg(feature = "memmap2")]
        if let Some(file) = &self._file {
            return self.get_doc_pread(file, doc_id);
        }
        self.get_doc_mmap(doc_id)
    }

    fn get_doc_mmap(&self, doc_id: u32) -> Option<DocEntry> {
        // Use checked arithmetic to avoid silent integer overflow on pathological
        // segments. doc_table_offset is validated at parse time (parse_segment_mmap
        // bounds-checks it), but a defence-in-depth check here costs nothing.
        let idx_pos = self
            .doc_table_offset
            .checked_add((doc_id as usize).checked_mul(8)?)?;
        let abs_off =
            u64::from_le_bytes(self.mmap.get(idx_pos..idx_pos + 8)?.try_into().ok()?) as usize;
        // Security: validate abs_off points within the doc table section, not the
        // dictionary or footer. Doc entries occupy [doc_table_offset, dict_offset).
        // Minimum fixed entry size: doc_id(4) + content_hash(8) + size_bytes(8) +
        // path_len(2) = 22 bytes. A crafted segment with a valid checksum could embed
        // an abs_off pointing into the dict section; without this check, dict bytes
        // would be returned to callers as DocEntry fields (information disclosure).
        const MIN_DOC_ENTRY_BYTES: usize = 22;
        if abs_off < self.doc_table_offset
            || abs_off.saturating_add(MIN_DOC_ENTRY_BYTES) > self.dict_offset
        {
            return None;
        }
        let e = self.mmap.get(abs_off..)?;
        let doc_id_r = u32::from_le_bytes(e.get(0..4)?.try_into().ok()?);
        let content_hash = u64::from_le_bytes(e.get(4..12)?.try_into().ok()?);
        let size_bytes = u64::from_le_bytes(e.get(12..20)?.try_into().ok()?);
        let path_len = u16::from_le_bytes(e.get(20..22)?.try_into().ok()?) as usize;
        // Security: verify the full variable-length entry (22 fixed bytes + path)
        // fits within the doc table region [doc_table_offset, dict_offset). The
        // earlier MIN_DOC_ENTRY_BYTES check only reserved space for the 22-byte
        // fixed header. A crafted segment could set path_len large enough to
        // extend the slice past dict_offset, silently dropping this doc from all
        // query results (targeted denial-of-service against specific files).
        if abs_off.saturating_add(22 + path_len) > self.dict_offset {
            return None;
        }
        let path = path_from_bytes(e.get(22..22 + path_len)?);
        Some(DocEntry {
            doc_id: doc_id_r,
            content_hash,
            size_bytes,
            path,
        })
    }

    /// `pread`-based equivalent of [`Self::get_doc_mmap`]: reads the doc-table
    /// index entry and the doc entry itself via positional reads against the
    /// open dict file instead of indexing into the mmap.
    ///
    /// Security: this is the same bounds-checked parsing as the mmap path
    /// (same range checks, same fixed/variable entry sizes), so it carries no
    /// new information-disclosure risk. The benefit is availability, not
    /// confidentiality: a `read_exact_at` past EOF returns `UnexpectedEof`
    /// (mapped to `None` here), whereas indexing a page past EOF on a mapping
    /// whose backing file was truncated after open delivers `SIGBUS` and
    /// kills the process. Reading through the file descriptor sidesteps that
    /// window entirely for these two call sites.
    #[cfg(feature = "memmap2")]
    fn get_doc_pread(&self, file: &std::fs::File, doc_id: u32) -> Option<DocEntry> {
        let idx_pos = self
            .doc_table_offset
            .checked_add((doc_id as usize).checked_mul(8)?)?;
        let mut idx_buf = [0u8; 8];
        reader::read_exact_at(file, &mut idx_buf, idx_pos as u64).ok()?;
        let abs_off = u64::from_le_bytes(idx_buf) as usize;

        // Security: same range check as get_doc_mmap — see that function's
        // comment for the rationale.
        const MIN_DOC_ENTRY_BYTES: usize = 22;
        if abs_off < self.doc_table_offset
            || abs_off.saturating_add(MIN_DOC_ENTRY_BYTES) > self.dict_offset
        {
            return None;
        }
        let mut header = [0u8; 22];
        reader::read_exact_at(file, &mut header, abs_off as u64).ok()?;
        let doc_id_r = u32::from_le_bytes(header[0..4].try_into().ok()?);
        let content_hash = u64::from_le_bytes(header[4..12].try_into().ok()?);
        let size_bytes = u64::from_le_bytes(header[12..20].try_into().ok()?);
        let path_len = u16::from_le_bytes(header[20..22].try_into().ok()?) as usize;

        // Security: same variable-length bounds check as get_doc_mmap.
        if abs_off.saturating_add(22 + path_len) > self.dict_offset {
            return None;
        }
        let mut path_buf = vec![0u8; path_len];
        let path_off = abs_off.checked_add(22)?;
        reader::read_exact_at(file, &mut path_buf, path_off as u64).ok()?;
        let path = path_from_bytes(&path_buf);
        Some(DocEntry {
            doc_id: doc_id_r,
            content_hash,
            size_bytes,
            path,
        })
    }

    /// Binary-search the dictionary for `gram_hash`, returning its posting
    /// offset and entry count. Dispatches to a `pread`-based lookup when a
    /// backing file is available, mirroring [`Self::get_doc`].
    pub(super) fn dict_lookup(&self, gram_hash: u64) -> Option<(usize, u32)> {
        #[cfg(feature = "memmap2")]
        if let Some(file) = &self._file {
            return self.dict_lookup_pread(file, gram_hash);
        }
        self.dict_lookup_mmap(gram_hash)
    }

    fn dict_lookup_mmap(&self, gram_hash: u64) -> Option<(usize, u32)> {
        let dict = self.mmap.get(self.dict_offset..)?;
        let n = self.gram_count as usize;
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let base = mid * DICT_ENTRY_SIZE;
            let mid_hash = u64::from_le_bytes(dict.get(base..base + 8)?.try_into().ok()?);
            match mid_hash.cmp(&gram_hash) {
                std::cmp::Ordering::Equal => {
                    let abs_off =
                        u64::from_le_bytes(dict.get(base + 8..base + 16)?.try_into().ok()?)
                            as usize;
                    let count =
                        u32::from_le_bytes(dict.get(base + 16..base + 20)?.try_into().ok()?);
                    return Some((abs_off, count));
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// `pread`-based equivalent of [`Self::dict_lookup_mmap`]. See
    /// [`Self::get_doc_pread`] for the security/availability rationale.
    #[cfg(feature = "memmap2")]
    fn dict_lookup_pread(&self, file: &std::fs::File, gram_hash: u64) -> Option<(usize, u32)> {
        let n = self.gram_count as usize;
        let mut lo = 0usize;
        let mut hi = n;
        let mut entry = [0u8; DICT_ENTRY_SIZE];
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let base = self
                .dict_offset
                .checked_add(mid.checked_mul(DICT_ENTRY_SIZE)?)?;
            reader::read_exact_at(file, &mut entry, base as u64).ok()?;
            let mid_hash = u64::from_le_bytes(entry[0..8].try_into().ok()?);
            match mid_hash.cmp(&gram_hash) {
                std::cmp::Ordering::Equal => {
                    let abs_off = u64::from_le_bytes(entry[8..16].try_into().ok()?) as usize;
                    let count = u32::from_le_bytes(entry[16..20].try_into().ok()?);
                    return Some((abs_off, count));
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }
}
