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

#[cfg(feature = "memmap2")]
use memmap2::Mmap;
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

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
    /// Byte length of the `.post` file as written. 0 for v2 segments.
    /// Recorded in the manifest for O(1) truncation detection at open time.
    pub post_len: u64,
    /// Sum of all document sizes in this segment.
    pub doc_bytes: u64,
}

mod segment_writer;
pub use segment_writer::SegmentWriter;

mod open;
pub use open::{DictVerify, PostVerify};
mod reader;
mod dict_read;

// ---------------------------------------------------------------------------
// T022: MmapSegment (reader)
// ---------------------------------------------------------------------------

/// Backing storage for a loaded segment's bytes.
///
/// On native targets the dict file is memory-mapped (zero-copy, lazy
/// fault-in). On WASM there is no mmap; bytes are heap-allocated instead.
pub(super) enum SegmentData {
    #[cfg(feature = "memmap2")]
    Mmap(Mmap),
    Heap(Vec<u8>),
}

impl std::ops::Deref for SegmentData {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            #[cfg(feature = "memmap2")]
            SegmentData::Mmap(m) => m,
            SegmentData::Heap(v) => v,
        }
    }
}

/// How postings are loaded: from the combined mmap (v2), a separate .post
/// file via pread (v3), or an in-memory Vec<u8> (WASM / tests).
pub(super) enum PostingsBacking {
    /// v2: postings data lives in the segment mmap at absolute file offsets.
    #[cfg(feature = "memmap2")]
    V2Mmap,
    /// v3: postings are in a separate .post file; offsets are from byte 0.
    #[cfg(feature = "memmap2")]
    V3File(std::fs::File),
    /// WASM / in-memory: entire postings file bytes (including SNTXPOST magic
    /// and checksum trailer) held in a heap Vec.
    InMemory(Vec<u8>),
}

/// Memory-mapped (native) or heap-backed (WASM) read-only SNTX segment.
///
/// On native targets, retains the open `File` handle so the OS keeps the inode
/// alive even if the directory entry is removed (e.g. by GC).
/// `expected_len` enables O(1) staleness detection on every read.
pub struct MmapSegment {
    pub(super) _file: Option<std::fs::File>,
    pub(super) mmap: SegmentData,
    pub(super) expected_len: usize,
    /// Number of documents in this segment.
    pub doc_count: u32,
    /// Number of distinct gram hashes in the dictionary.
    pub gram_count: u32,
    pub(super) doc_table_offset: usize,
    pub(super) dict_offset: usize,
    /// Conservative lower bound for postings in V2 mmap reads. 0 for V3.
    #[cfg_attr(not(feature = "memmap2"), allow(dead_code))]
    pub(super) postings_start: usize,
    pub(super) postings: PostingsBacking,
    /// Sum of all document sizes in this segment.
    pub doc_bytes: Option<u64>,
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

    #[cfg_attr(not(feature = "memmap2"), allow(dead_code))]
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

    fn read_posting_list(&self, abs_off: usize) -> Option<PostingList> {
        match &self.postings {
            #[cfg(feature = "memmap2")]
            PostingsBacking::V2Mmap => self.read_posting_list_mmap(abs_off),
            #[cfg(feature = "memmap2")]
            PostingsBacking::V3File(post_file) => {
                reader::read_posting_list_pread(post_file, abs_off as u64).ok()
            }
            PostingsBacking::InMemory(bytes) => {
                // bytes layout: [SNTXPOST magic (8)] [postings data] [checksum (8)]
                // abs_off is relative to the start of postings data (after magic).
                use crate::posting::PostingList;
                const POST_MAGIC_SIZE: usize = 8;
                let off = POST_MAGIC_SIZE + abs_off;
                let b = bytes.get(off..)?;
                // Entry header: encoding(1) + count(4) + byte_len(4) = 9 bytes
                const MIN_POSTING_BYTES: usize = 9;
                if b.len() < MIN_POSTING_BYTES {
                    return None;
                }
                let encoding = b[0];
                let byte_len = u32::from_le_bytes(b[5..9].try_into().ok()?) as usize;
                let data = b.get(9..9 + byte_len)?;
                match encoding {
                    0 => Some(PostingList::Small(data.to_vec())),
                    1 => roaring_util::deserialize(data).ok().map(PostingList::Large),
                    _ => None,
                }
            }
        }
    }

    #[cfg_attr(not(feature = "memmap2"), allow(dead_code))]
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
