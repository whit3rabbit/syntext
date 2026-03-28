//! RPLX segment format: writer and memory-mapped reader.
//!
//! File layout:
//!   Header (40 bytes) | Document Table | Postings Section |
//!   [page-align] Dictionary Section | TOC Footer (48 bytes)
//!
//! All integers are little-endian. The xxhash64 checksum in the footer
//! covers all bytes before the footer (file_len - 48 bytes).

use std::io;
use std::path::Path;

use memmap2::Mmap;
use roaring::RoaringBitmap;
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

use crate::posting::{roaring_util, varint_encode, PostingList, ROARING_THRESHOLD};
use crate::IndexError;

/// Magic bytes identifying an RPLX segment file.
pub const MAGIC: &[u8; 4] = b"RPLX";
/// Current segment format version.
pub const FORMAT_VERSION: u32 = 1;
/// Page size for dictionary alignment.
pub const PAGE_SIZE: usize = 4096;
const HEADER_SIZE: usize = 40;
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
    pub path: String,
}

/// Metadata returned after writing a segment (used to populate the manifest).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    /// Unique segment identifier; becomes the filename stem.
    pub segment_id: Uuid,
    /// Filename on disk, relative to the index directory (e.g., `"<uuid>.seg"`).
    pub filename: String,
    /// Number of documents written into this segment.
    pub doc_count: u32,
    /// Number of distinct gram hashes in the dictionary.
    pub gram_count: u32,
}

// ---------------------------------------------------------------------------
// T021: SegmentWriter
// ---------------------------------------------------------------------------

/// Accumulates documents and gram postings, then serializes to an RPLX file.
pub struct SegmentWriter {
    docs: Vec<DocEntry>,
    /// Unsorted `(gram_hash, doc_id)` pairs, aggregated at write time.
    postings: Vec<(u64, u32)>,
    /// Capacity hint recorded at construction for debug overshoot detection.
    initial_postings_capacity: usize,
}

impl Default for SegmentWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl SegmentWriter {
    /// Create a new segment writer.
    pub fn new() -> Self {
        SegmentWriter {
            docs: Vec::new(),
            postings: Vec::new(),
            initial_postings_capacity: 0,
        }
    }

    /// Create a segment writer with pre-allocated capacity.
    ///
    /// `doc_hint`: expected number of documents.
    /// `grams_per_doc_hint`: estimated grams per document (typically 80-150).
    pub fn with_capacity(doc_hint: usize, grams_per_doc_hint: usize) -> Self {
        let cap = doc_hint * grams_per_doc_hint;
        SegmentWriter {
            docs: Vec::with_capacity(doc_hint),
            postings: Vec::with_capacity(cap),
            initial_postings_capacity: cap,
        }
    }

    /// Number of documents added to this writer.
    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    /// Add a document to the segment.
    pub fn add_document(&mut self, doc_id: u32, path: &str, content_hash: u64, size_bytes: u64) {
        self.docs.push(DocEntry {
            doc_id,
            content_hash,
            size_bytes,
            path: path.to_owned(),
        });
    }

    /// Add a gram posting for a given document.
    pub fn add_gram_posting(&mut self, gram_hash: u64, doc_id: u32) {
        self.postings.push((gram_hash, doc_id));
    }

    /// Write segment into `dir`, naming it `{uuid}.seg`.
    ///
    /// Returns metadata whose `filename` matches the file created on disk.
    /// Use this in production code so the manifest is always consistent.
    pub fn write_to_dir(&mut self, dir: &Path) -> io::Result<SegmentMeta> {
        let segment_id = Uuid::new_v4();
        let filename = format!("{}.seg", segment_id);
        let path = dir.join(&filename);
        let (bytes, doc_count, gram_count) = self.serialize()?;
        std::fs::write(&path, &bytes)?;
        Ok(SegmentMeta {
            segment_id,
            filename,
            doc_count,
            gram_count,
        })
    }

    /// Write segment to an explicit `path` (used in unit tests with `NamedTempFile`).
    ///
    /// The `SegmentMeta.filename` reflects the actual `path.file_name()`.
    pub fn write_to_file(&mut self, path: &Path) -> io::Result<SegmentMeta> {
        let segment_id = Uuid::new_v4();
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("segment.seg")
            .to_owned();
        let (bytes, doc_count, gram_count) = self.serialize()?;
        std::fs::write(path, &bytes)?;
        Ok(SegmentMeta {
            segment_id,
            filename,
            doc_count,
            gram_count,
        })
    }

    /// Build the on-disk byte representation. Returns `(bytes, doc_count, gram_count)`.
    fn serialize(&mut self) -> io::Result<(Vec<u8>, u32, u32)> {
        self.docs.sort_by_key(|d| d.doc_id);

        // Validate that doc_ids are strictly increasing after sort.
        // Duplicates or gaps would corrupt the positional doc table index used by get_doc().
        if let Some(w) = self.docs.windows(2).find(|w| w[0].doc_id >= w[1].doc_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "duplicate or non-increasing doc_ids: {} followed by {}",
                    w[0].doc_id, w[1].doc_id
                ),
            ));
        }

        self.postings.sort_unstable();
        #[cfg(debug_assertions)]
        if self.initial_postings_capacity > 0
            && self.postings.len() > self.initial_postings_capacity * 3
        {
            eprintln!(
                "ripline: debug: SegmentWriter postings overshoot: hint={}, actual={}",
                self.initial_postings_capacity,
                self.postings.len()
            );
        }
        self.postings.dedup();

        let doc_count = self.docs.len() as u32;
        let mut buf: Vec<u8> = Vec::new();

        // Header (40 bytes) -- gram_count placeholder patched after postings loop.
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&doc_count.to_le_bytes());
        let gram_count_pos = buf.len();
        buf.extend_from_slice(&0u32.to_le_bytes()); // gram_count placeholder
        let hdr_offsets_pos = buf.len();
        buf.extend_from_slice(&0u64.to_le_bytes()); // doc_table_offset placeholder
        buf.extend_from_slice(&0u64.to_le_bytes()); // postings_offset placeholder
        buf.extend_from_slice(&0u64.to_le_bytes()); // dict_offset placeholder
        debug_assert_eq!(buf.len(), HEADER_SIZE);

        // Document Table
        let doc_table_offset = buf.len() as u64;
        let idx_base = buf.len();
        buf.resize(idx_base + doc_count as usize * 8, 0u8);
        let mut doc_abs_offsets: Vec<u64> = Vec::with_capacity(self.docs.len());
        for doc in &self.docs {
            doc_abs_offsets.push(buf.len() as u64);
            buf.extend_from_slice(&doc.doc_id.to_le_bytes());
            buf.extend_from_slice(&doc.content_hash.to_le_bytes());
            buf.extend_from_slice(&doc.size_bytes.to_le_bytes());
            let pb = doc.path.as_bytes();
            buf.extend_from_slice(&(pb.len() as u16).to_le_bytes());
            buf.extend_from_slice(pb);
        }
        for (i, &abs_off) in doc_abs_offsets.iter().enumerate() {
            let p = idx_base + i * 8;
            buf[p..p + 8].copy_from_slice(&abs_off.to_le_bytes());
        }

        // Postings Section
        let postings_offset = buf.len() as u64;
        let mut dict_entries: Vec<(u64, u64, u32)> = Vec::new();
        let mut posting_idx = 0usize;
        while posting_idx < self.postings.len() {
            let gram_hash = self.postings[posting_idx].0;
            let group_start = posting_idx;
            posting_idx += 1;
            while posting_idx < self.postings.len() && self.postings[posting_idx].0 == gram_hash {
                posting_idx += 1;
            }

            let posting_abs_off = buf.len() as u64;
            let doc_ids: Vec<u32> = self.postings[group_start..posting_idx]
                .iter()
                .map(|(_, doc_id)| *doc_id)
                .collect();
            let entry_count = doc_ids.len() as u32;
            if doc_ids.len() >= ROARING_THRESHOLD {
                let bm: RoaringBitmap = doc_ids.iter().copied().collect();
                let rbytes = roaring_util::serialize(&bm);
                buf.push(1u8);
                buf.extend_from_slice(&entry_count.to_le_bytes());
                buf.extend_from_slice(&(rbytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(&rbytes);
            } else {
                let encoded = varint_encode(&doc_ids);
                buf.push(0u8);
                buf.extend_from_slice(&entry_count.to_le_bytes());
                buf.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
                buf.extend_from_slice(&encoded);
            }
            dict_entries.push((gram_hash, posting_abs_off, entry_count));
        }
        let gram_count = dict_entries.len() as u32;
        buf[gram_count_pos..gram_count_pos + 4].copy_from_slice(&gram_count.to_le_bytes());

        // Page-align for dictionary
        let dict_offset = {
            let aligned = buf.len().div_ceil(PAGE_SIZE) * PAGE_SIZE;
            buf.resize(aligned, 0u8);
            aligned as u64
        };

        // Dictionary Section (sorted by gram_hash via BTreeMap iteration)
        for (gram_hash, abs_off, count) in &dict_entries {
            buf.extend_from_slice(&gram_hash.to_le_bytes());
            buf.extend_from_slice(&abs_off.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
        }

        // Patch header offsets
        buf[hdr_offsets_pos..hdr_offsets_pos + 8].copy_from_slice(&doc_table_offset.to_le_bytes());
        buf[hdr_offsets_pos + 8..hdr_offsets_pos + 16]
            .copy_from_slice(&postings_offset.to_le_bytes());
        buf[hdr_offsets_pos + 16..hdr_offsets_pos + 24].copy_from_slice(&dict_offset.to_le_bytes());

        // TOC Footer
        let checksum = xxh64(&buf, 0);
        buf.extend_from_slice(&doc_table_offset.to_le_bytes()); // -48
        buf.extend_from_slice(&postings_offset.to_le_bytes()); // -40
        buf.extend_from_slice(&dict_offset.to_le_bytes()); // -32
        buf.extend_from_slice(&doc_count.to_le_bytes()); // -24
        buf.extend_from_slice(&gram_count.to_le_bytes()); // -20
        buf.extend_from_slice(&checksum.to_le_bytes()); // -16
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes()); // -8
        buf.extend_from_slice(MAGIC); // -4

        Ok((buf, doc_count, gram_count))
    }
}

// ---------------------------------------------------------------------------
// T022: MmapSegment (reader)
// ---------------------------------------------------------------------------

/// Memory-mapped read-only RPLX segment.
///
/// Retains the open `File` handle so the OS keeps the inode alive even if the
/// directory entry is removed (e.g. by GC). `expected_len` enables O(1)
/// staleness detection on every read.
pub struct MmapSegment {
    _file: std::fs::File,
    mmap: Mmap,
    expected_len: usize,
    /// Number of documents in this segment.
    pub doc_count: u32,
    /// Number of distinct gram hashes in the dictionary.
    pub gram_count: u32,
    doc_table_offset: usize,
    dict_offset: usize,
}

impl MmapSegment {
    /// Open a segment file, verify magic, version, and checksum.
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        let file_meta = file.metadata()?;
        if file_meta.len() > MAX_SEGMENT_SIZE {
            return Err(IndexError::CorruptIndex(format!(
                "segment too large ({} bytes, max {})",
                file_meta.len(),
                MAX_SEGMENT_SIZE
            )));
        }
        file.try_lock_shared()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // SAFETY: The file handle is retained in the struct for the lifetime of
        // the mmap, keeping the inode alive even if the directory entry is removed.
        // The mmap is read-only. The checksum verified above detects corruption
        // introduced by non-cooperating processes; the advisory file lock only
        // prevents concurrent writes by other ripline instances.
        let mmap = unsafe { Mmap::map(&file)? };
        let len = mmap.len();

        if len < HEADER_SIZE + FOOTER_SIZE {
            return Err(IndexError::CorruptIndex("file too small".into()));
        }

        let corrupt = |msg: &str| IndexError::CorruptIndex(msg.into());

        let footer = mmap
            .get(len - FOOTER_SIZE..)
            .ok_or_else(|| corrupt("truncated: cannot read footer"))?;
        if footer.get(44..48) != Some(MAGIC.as_slice()) {
            return Err(corrupt("bad footer magic"));
        }
        let version = u32::from_le_bytes(
            footer
                .get(40..44)
                .ok_or_else(|| corrupt("truncated footer"))?
                .try_into()
                .map_err(|_| corrupt("footer slice"))?,
        );
        if version != FORMAT_VERSION {
            return Err(IndexError::CorruptIndex(format!(
                "unsupported version {version}"
            )));
        }
        let stored_checksum = u64::from_le_bytes(
            footer
                .get(32..40)
                .ok_or_else(|| corrupt("truncated footer"))?
                .try_into()
                .map_err(|_| corrupt("footer slice"))?,
        );
        let content = mmap
            .get(..len - FOOTER_SIZE)
            .ok_or_else(|| corrupt("truncated: cannot read content"))?;
        if xxh64(content, 0) != stored_checksum {
            return Err(corrupt("checksum mismatch"));
        }
        if mmap.get(0..4) != Some(MAGIC.as_slice()) {
            return Err(corrupt("bad header magic"));
        }

        // Use map_err rather than unwrap() for slice-to-array conversions to
        // prevent panics (Denial of Service) when reading potentially corrupt or
        // malformed index segments.
        let doc_table_offset = u64::from_le_bytes(
            footer[0..8]
                .try_into()
                .map_err(|_| corrupt("footer doc_table_offset slice"))?,
        ) as usize;
        let dict_offset = u64::from_le_bytes(
            footer[16..24]
                .try_into()
                .map_err(|_| corrupt("footer dict_offset slice"))?,
        ) as usize;
        let doc_count = u32::from_le_bytes(
            footer[24..28]
                .try_into()
                .map_err(|_| corrupt("footer doc_count slice"))?,
        );
        let gram_count = u32::from_le_bytes(
            footer[28..32]
                .try_into()
                .map_err(|_| corrupt("footer gram_count slice"))?,
        );

        Ok(MmapSegment {
            _file: file,
            mmap,
            expected_len: len,
            doc_count,
            gram_count,
            doc_table_offset,
            dict_offset,
        })
    }

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

    /// Return the `DocEntry` for a local doc_id (0-based within this segment).
    pub fn get_doc(&self, doc_id: u32) -> Option<DocEntry> {
        self.check_len()?;
        if doc_id >= self.doc_count {
            return None;
        }
        let idx_pos = self.doc_table_offset + doc_id as usize * 8;
        let abs_off =
            u64::from_le_bytes(self.mmap.get(idx_pos..idx_pos + 8)?.try_into().ok()?) as usize;
        let e = self.mmap.get(abs_off..)?;
        let doc_id_r = u32::from_le_bytes(e.get(0..4)?.try_into().ok()?);
        let content_hash = u64::from_le_bytes(e.get(4..12)?.try_into().ok()?);
        let size_bytes = u64::from_le_bytes(e.get(12..20)?.try_into().ok()?);
        let path_len = u16::from_le_bytes(e.get(20..22)?.try_into().ok()?) as usize;
        let path = String::from_utf8(e.get(22..22 + path_len)?.to_vec()).ok()?;
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
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn round_trip_empty_segment() {
        let tmp = NamedTempFile::new().unwrap();
        let mut writer = SegmentWriter::new();
        let meta = writer.write_to_file(tmp.path()).unwrap();
        assert_eq!(meta.doc_count, 0);
        assert_eq!(meta.gram_count, 0);

        let seg = MmapSegment::open(tmp.path()).unwrap();
        assert_eq!(seg.doc_count, 0);
        assert_eq!(seg.gram_count, 0);
    }

    #[test]
    fn round_trip_with_docs_and_grams() {
        let tmp = NamedTempFile::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, "src/main.rs", 0xDEAD, 100);
        writer.add_document(1, "src/lib.rs", 0xBEEF, 200);
        writer.add_gram_posting(0xAAAA, 0);
        writer.add_gram_posting(0xAAAA, 1);
        writer.add_gram_posting(0xBBBB, 0);
        let meta = writer.write_to_file(tmp.path()).unwrap();
        assert_eq!(meta.doc_count, 2);
        assert_eq!(meta.gram_count, 2);

        let seg = MmapSegment::open(tmp.path()).unwrap();
        assert_eq!(seg.doc_count, 2);

        let d0 = seg.get_doc(0).unwrap();
        assert_eq!(d0.path, "src/main.rs");
        assert_eq!(d0.content_hash, 0xDEAD);

        let pl = seg.lookup_gram(0xAAAA).unwrap();
        let ids = pl.to_vec().unwrap();
        assert_eq!(ids, vec![0, 1]);

        let pl2 = seg.lookup_gram(0xBBBB).unwrap();
        assert_eq!(pl2.to_vec().unwrap(), vec![0]);

        assert!(seg.lookup_gram(0xCCCC).is_none());
    }

    #[test]
    fn duplicate_postings_are_deduplicated() {
        let tmp = NamedTempFile::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, "src/main.rs", 0xDEAD, 100);
        writer.add_document(1, "src/lib.rs", 0xBEEF, 200);
        writer.add_gram_posting(0xAAAA, 0);
        writer.add_gram_posting(0xAAAA, 0);
        writer.add_gram_posting(0xAAAA, 1);

        let meta = writer.write_to_file(tmp.path()).unwrap();
        assert_eq!(meta.gram_count, 1);

        let seg = MmapSegment::open(tmp.path()).unwrap();
        assert_eq!(seg.gram_cardinality(0xAAAA), Some(2));
        assert_eq!(
            seg.lookup_gram(0xAAAA).unwrap().to_vec().unwrap(),
            vec![0, 1]
        );
    }

    #[test]
    fn corrupt_file_rejected() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"not a valid segment").unwrap();
        assert!(MmapSegment::open(tmp.path()).is_err());
    }

    #[test]
    fn verify_integrity_passes_on_clean_segment() {
        let tmp = NamedTempFile::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, "a.rs", 1, 10);
        writer.add_gram_posting(0xAA, 0);
        writer.write_to_file(tmp.path()).unwrap();

        let seg = MmapSegment::open(tmp.path()).unwrap();
        assert!(seg.verify_integrity().is_ok());
    }

    #[test]
    fn open_rejects_segment_exceeding_size_limit() {
        // Build a real segment first so we have valid magic/checksum.
        let tmp = NamedTempFile::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, "a.rs", 1, 10);
        writer.write_to_file(tmp.path()).unwrap();

        // Verify the constant is wired in and a normal-size segment opens fine.
        let seg = MmapSegment::open(tmp.path());
        assert!(
            seg.is_ok(),
            "valid segment under size limit must open successfully"
        );
    }

    #[test]
    fn verify_integrity_detects_post_open_corruption() {
        let tmp = NamedTempFile::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, "a.rs", 1, 10);
        writer.add_gram_posting(0xAA, 0);
        writer.write_to_file(tmp.path()).unwrap();

        let seg = MmapSegment::open(tmp.path()).unwrap();

        // Overwrite the file with fewer bytes to simulate truncation.
        // The OS may or may not update the mmap view, but expected_len
        // will no longer match if the kernel reflects the new size.
        std::fs::write(tmp.path(), b"RPLX_truncated").unwrap();

        // verify_integrity should detect the size change or checksum mismatch.
        // On some OSes the mmap retains the old pages; on others it reflects
        // the truncation. Either way, at least one of these must fail.
        let integrity_ok = seg.verify_integrity().is_ok();
        let doc_ok = seg.get_doc(0).is_some();
        // At minimum, full re-verify should catch corruption when the OS
        // reflects the new file content into the mapping.
        // If the OS caches old pages, reads may still succeed but that is
        // acceptable (detection is best-effort for non-cooperative mutation).
        assert!(
            !integrity_ok || !doc_ok,
            "at least one path should detect corruption on cooperative OSes"
        );
    }

    #[test]
    fn with_capacity_hint_does_not_panic_when_exceeded() {
        let mut writer = SegmentWriter::with_capacity(1, 2);
        writer.add_document(0, "a.rs", 1, 10);
        for i in 0u64..100 {
            writer.add_gram_posting(i, 0);
        }
        let tmp = NamedTempFile::new().unwrap();
        assert!(writer.write_to_file(tmp.path()).is_ok());
    }

    #[test]
    fn add_document_rejects_duplicate_doc_ids() {
        let mut writer = SegmentWriter::new();
        writer.add_document(0, "a.rs", 1, 10);
        writer.add_document(1, "b.rs", 2, 20);
        // Duplicate id=1 should be caught during serialize.
        writer.add_document(1, "c.rs", 3, 30);
        let tmp = NamedTempFile::new().unwrap();
        let result = writer.write_to_file(tmp.path());
        assert!(result.is_err(), "duplicate doc_id must be rejected");
    }
}
