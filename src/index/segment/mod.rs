//! SNTX segment format: writer and memory-mapped reader.
//!
//! File layout:
//!   Header (40 bytes) | Document Table | Postings Section |
//!   [page-align] Dictionary Section | TOC Footer (48 bytes)
//!
//! All integers are little-endian. The xxhash64 checksum in the footer
//! covers all bytes before the footer (file_len - 48 bytes).
//!
//! V3 format splits into `{uuid}.dict` (header + doc table + dictionary) and
//! `{uuid}.post` (postings). See `open_split()`.

use memmap2::Mmap;
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

use crate::path_util::path_from_bytes;
use crate::posting::{roaring_util, PostingList};
use crate::IndexError;

/// Magic bytes identifying an SNTX segment file.
pub const MAGIC: &[u8; 4] = b"SNTX";
/// Segment format version 2: single combined `.seg` file (legacy).
pub const FORMAT_VERSION_V2: u32 = 2;
/// Segment format version 3: split `.dict` + `.post` files.
pub const FORMAT_VERSION_V3: u32 = 3;
/// Current default format version for new segments.
pub const FORMAT_VERSION: u32 = FORMAT_VERSION_V3;
/// Page size for dictionary alignment.
pub const PAGE_SIZE: usize = 4096;
pub(super) const HEADER_SIZE: usize = 40;
/// Size of the segment footer in bytes.
pub const FOOTER_SIZE: usize = 48;
/// Size of a single dictionary entry in bytes.
pub const DICT_ENTRY_SIZE: usize = 20;
/// Maximum segment file size. A 256MB source batch with overhead should never
/// produce a segment larger than this. Reject oversized files before mmap to
/// prevent a malicious .seg from exhausting virtual memory.
/// Set to 1 GB: 4x the maximum batch size, leaving headroom for worst-case
/// overhead while preventing runaway virtual-memory consumption.
pub const MAX_SEGMENT_SIZE: u64 = 1024 * 1024 * 1024; // 1 GB

/// Document metadata stored in a segment's document table.
#[derive(Debug, Clone)]
pub struct DocEntry {
    /// Segment-local document ID (0-based; globally unique when combined with segment UUID).
    pub doc_id: u32,
    /// xxHash-64 of the file's raw bytes; used for change detection during incremental updates.
    pub content_hash: u64,
    /// File size in bytes at index time.
    pub size_bytes: u64,
    /// Repository-relative path with forward-slash separators.
    pub path: std::path::PathBuf,
}

/// Metadata returned after writing a segment (used to populate the manifest).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    /// Unique segment identifier; becomes the filename stem.
    pub segment_id: Uuid,
    /// Legacy combined filename (`<uuid>.seg`). Empty for v3 segments.
    pub filename: String,
    /// Dictionary filename (`<uuid>.dict`) for v3 segments. Empty for v2.
    pub dict_filename: String,
    /// Postings filename (`<uuid>.post`) for v3 segments. Empty for v2.
    pub post_filename: String,
    /// Number of documents written into this segment.
    pub doc_count: u32,
    /// Number of distinct gram hashes in the dictionary.
    pub gram_count: u32,
}

mod segment_writer;
pub use segment_writer::SegmentWriter;

mod reader;

// ---------------------------------------------------------------------------
// T022: MmapSegment (reader)
// ---------------------------------------------------------------------------

/// How postings are loaded: from the combined mmap (v2) or a separate .post
/// file via pread (v3).
pub(super) enum PostingsBacking {
    /// v2: postings data lives in the segment mmap at absolute file offsets.
    V2Mmap,
    /// v3: postings are in a separate .post file; offsets are from byte 0.
    V3File(std::fs::File),
}

/// Memory-mapped read-only SNTX segment.
///
/// Retains the open `File` handle so the OS keeps the inode alive even if the
/// directory entry is removed (e.g. by GC). `expected_len` enables O(1)
/// staleness detection on every read.
pub struct MmapSegment {
    pub(super) _file: std::fs::File,
    pub(super) mmap: Mmap,
    pub(super) expected_len: usize,
    /// Number of documents in this segment.
    pub doc_count: u32,
    /// Number of distinct gram hashes in the dictionary.
    pub gram_count: u32,
    pub(super) doc_table_offset: usize,
    pub(super) dict_offset: usize,
    /// Conservative lower bound for postings in V2 mmap reads. 0 for V3.
    pub(super) postings_start: usize,
    pub(super) postings: PostingsBacking,
}

impl MmapSegment {
    /// O(1) check that the underlying file has not been truncated or extended
    /// since the segment was opened. Returns `None` if the mmap length changed.
    fn check_len(&self) -> Option<()> {
        if self.mmap.len() == self.expected_len {
            Some(())
        } else {
            None
        }
    }

    /// Re-verify the segment checksum. O(file_size), not intended for per-query
    /// use. Returns `Ok(())` if the file is intact.
    pub fn verify_integrity(&self) -> Result<(), IndexError> {
        let len = self.mmap.len();
        if len != self.expected_len {
            return Err(IndexError::CorruptIndex(format!(
                "segment size changed: expected {}, got {}",
                self.expected_len, len,
            )));
        }
        let content = self
            .mmap
            .get(..len - FOOTER_SIZE)
            .ok_or_else(|| IndexError::CorruptIndex("truncated".into()))?;
        let footer = self
            .mmap
            .get(len - FOOTER_SIZE..)
            .ok_or_else(|| IndexError::CorruptIndex("truncated".into()))?;
        let stored = u64::from_le_bytes(
            footer
                .get(32..40)
                .ok_or_else(|| IndexError::CorruptIndex("truncated footer".into()))?
                .try_into()
                .map_err(|_| IndexError::CorruptIndex("footer slice".into()))?,
        );
        if xxh64(content, 0) != stored {
            return Err(IndexError::CorruptIndex(
                "checksum mismatch on re-verify".into(),
            ));
        }
        Ok(())
    }

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

    pub(crate) fn gram_hashes(&self) -> Result<Vec<u64>, IndexError> {
        self.check_len()
            .ok_or_else(|| IndexError::CorruptIndex("segment length changed".into()))?;
        let dict_len = (self.gram_count as usize)
            .checked_mul(DICT_ENTRY_SIZE)
            .ok_or_else(|| IndexError::CorruptIndex("dictionary size overflow".into()))?;
        let dict = self
            .mmap
            .get(self.dict_offset..self.dict_offset.saturating_add(dict_len))
            .ok_or_else(|| IndexError::CorruptIndex("truncated dictionary".into()))?;

        let mut hashes = Vec::with_capacity(self.gram_count as usize);
        for entry in dict.chunks_exact(DICT_ENTRY_SIZE) {
            hashes.push(u64::from_le_bytes(entry[0..8].try_into().map_err(
                |_| IndexError::CorruptIndex("dictionary entry hash".into()),
            )?));
        }
        Ok(hashes)
    }

    /// Return the `DocEntry` for a local doc_id (0-based within this segment).
    pub fn get_doc(&self, doc_id: u32) -> Option<DocEntry> {
        self.check_len()?;
        if doc_id >= self.doc_count {
            return None;
        }
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

    fn dict_lookup(&self, gram_hash: u64) -> Option<(usize, u32)> {
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

    fn read_posting_list(&self, abs_off: usize) -> Option<PostingList> {
        match &self.postings {
            PostingsBacking::V2Mmap => self.read_posting_list_mmap(abs_off),
            PostingsBacking::V3File(post_file) => {
                reader::read_posting_list_pread(post_file, abs_off as u64).ok()
            }
        }
    }

    fn read_posting_list_mmap(&self, abs_off: usize) -> Option<PostingList> {
        // Security: validate abs_off points within the postings section of a V2
        // combined segment. Postings precede the dictionary; minimum entry size is
        // 9 bytes: encoding(1) + count(4) + byte_len(4). Without this check, a
        // crafted V2 dict entry with an abs_off pointing into the doc table or
        // header would return garbage bytes as a posting list (information disclosure).
        const MIN_POSTING_BYTES: usize = 9;
        if abs_off < self.postings_start
            || abs_off.saturating_add(MIN_POSTING_BYTES) > self.dict_offset
        {
            return None;
        }
        let b = self.mmap.get(abs_off..)?;
        let encoding = *b.first()?;
        let byte_len = u32::from_le_bytes(b.get(5..9)?.try_into().ok()?) as usize;
        let data = b.get(9..9 + byte_len)?;
        match encoding {
            0 => Some(PostingList::Small(data.to_vec())),
            1 => roaring_util::deserialize(data).ok().map(PostingList::Large),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
