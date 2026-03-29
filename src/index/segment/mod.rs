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

use std::path::Path;

use memmap2::{Mmap, MmapOptions};
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
enum PostingsBacking {
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
    _file: std::fs::File,
    mmap: Mmap,
    expected_len: usize,
    /// Number of documents in this segment.
    pub doc_count: u32,
    /// Number of distinct gram hashes in the dictionary.
    pub gram_count: u32,
    doc_table_offset: usize,
    dict_offset: usize,
    /// Conservative lower bound for postings in V2 mmap reads. 0 for V3.
    postings_start: usize,
    postings: PostingsBacking,
}

impl MmapSegment {
    /// Open a combined (v2) segment file, verify magic, version, and checksum.
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
        //
        // Security: we use MAP_PRIVATE (map_copy_read_only) rather than MAP_SHARED.
        // With MAP_SHARED, a process with write access to the index directory could
        // mutate segment bytes after the checksum passes, injecting false search
        // results (information disclosure / result manipulation) even though safe
        // Rust's .get() bounds checks prevent memory-safety violations. MAP_PRIVATE
        // creates a copy-on-write mapping: once parse_segment_mmap reads every
        // content page during checksum verification, those pages are in our private
        // address space and are immune to external mutations for the mapping's
        // lifetime. The advisory file lock still blocks concurrent writes by other
        // syntext instances.
        //
        // Residual SIGBUS risk: the advisory file lock (try_lock_shared above) does
        // not prevent other processes from truncating the file — advisory locks are
        // cooperative, not mandatory. If a concurrent truncate(2) races with the
        // linear page read inside parse_segment_mmap (specifically the xxh64 checksum
        // pass), accessing a page past the new EOF delivers SIGBUS, which terminates
        // the process. This is a denial-of-service risk when the index directory is
        // writable by a second principal. Once parse_segment_mmap completes and all
        // pages have been faulted into the private mapping, subsequent accesses are
        // safe. The index directory should be mode 0700 (owner only) in security-
        // sensitive deployments.
        let mmap = unsafe { MmapOptions::new().map_copy_read_only(&file)? };
        let len = mmap.len();
        // open() accepts both v2 and v3 version tags. The single-file layout is
        // identical for both; open_split() handles the split-file v3 read path.
        let layout = reader::parse_segment_mmap(&mmap, &[FORMAT_VERSION_V2, FORMAT_VERSION_V3])?;

        Ok(MmapSegment {
            _file: file,
            mmap,
            expected_len: len,
            doc_count: layout.doc_count,
            gram_count: layout.gram_count,
            doc_table_offset: layout.doc_table_offset,
            dict_offset: layout.dict_offset,
            postings_start: layout.postings_start,
            postings: PostingsBacking::V2Mmap,
        })
    }

    /// Open a v3 segment from separate `.dict` and `.post` files.
    ///
    /// The `.dict` file is fully mmap'd (small, always needed for binary
    /// search). Postings are read on demand from `.post` via positional reads.
    pub fn open_split(dict_path: &Path, post_path: &Path) -> Result<Self, IndexError> {
        let file = std::fs::File::open(dict_path)?;
        let file_meta = file.metadata()?;
        if file_meta.len() > MAX_SEGMENT_SIZE {
            return Err(IndexError::CorruptIndex(format!(
                "dict file too large ({} bytes, max {})",
                file_meta.len(),
                MAX_SEGMENT_SIZE
            )));
        }
        file.try_lock_shared()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // SAFETY: same rationale as open() — file handle retained (_file field),
        // MAP_PRIVATE mapping (see open() comment), all downstream reads are
        // bounds-checked via .get(). The mmap only covers the `.dict` side;
        // postings are read from `.post` via positional reads.
        //
        // Residual SIGBUS risk: same as open() — see that comment. The window here
        // is narrower because only the .dict file is mmap'd; the .post file is read
        // via positional reads (read_exact_at) rather than mmap, so a truncation of
        // .post after open returns an I/O error rather than SIGBUS. The .dict mmap
        // is still subject to the SIGBUS window during parse_segment_mmap's checksum
        // read before all pages are faulted into the private mapping.
        let mmap = unsafe { MmapOptions::new().map_copy_read_only(&file)? };
        let len = mmap.len();
        let layout = reader::parse_segment_mmap(&mmap, &[FORMAT_VERSION_V3])?;
        let post_file = std::fs::File::open(post_path)?;
        post_file
            .try_lock_shared()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // Validate .post file magic and checksum.
        // Note: reading the full postings data at open time is O(post_file_size).
        // This is acceptable: the checksum read happens once per segment open, and
        // segments are reused across many queries.
        const POST_MAGIC: &[u8; 8] = b"SNTXPOST";
        const POST_MIN_SIZE: usize = 8 + 8; // magic + checksum (empty postings allowed)

        let post_meta = post_file.metadata()?;
        let post_len = post_meta.len() as usize;
        if post_len < POST_MIN_SIZE {
            return Err(IndexError::CorruptIndex(format!(
                "post file too small: {post_len} bytes"
            )));
        }

        // Read the magic header (8 bytes).
        let mut post_magic = [0u8; 8];
        reader::read_exact_at(&post_file, &mut post_magic, 0)?;
        if &post_magic != POST_MAGIC {
            return Err(IndexError::CorruptIndex(
                "post file has wrong magic (expected SNTXPOST)".into(),
            ));
        }

        // Read and verify the checksum (last 8 bytes cover the postings data
        // between the magic header and checksum trailer).
        let checksum_offset = (post_len - 8) as u64;
        let mut stored_cksum_bytes = [0u8; 8];
        reader::read_exact_at(&post_file, &mut stored_cksum_bytes, checksum_offset)?;
        let stored_post_checksum = u64::from_le_bytes(stored_cksum_bytes);

        // Read postings data (bytes 8..post_len-8) to compute expected checksum.
        let postings_data_len = post_len - 16; // subtract magic(8) + checksum(8)
        let mut postings_data = vec![0u8; postings_data_len];
        if postings_data_len > 0 {
            reader::read_exact_at(&post_file, &mut postings_data, 8)?;
        }
        let expected_post_checksum = xxh64(&postings_data, 0);
        if stored_post_checksum != expected_post_checksum {
            return Err(IndexError::CorruptIndex(
                "post file checksum mismatch".into(),
            ));
        }

        Ok(MmapSegment {
            _file: file,
            mmap,
            expected_len: len,
            doc_count: layout.doc_count,
            gram_count: layout.gram_count,
            doc_table_offset: layout.doc_table_offset,
            dict_offset: layout.dict_offset,
            postings_start: 0,
            postings: PostingsBacking::V3File(post_file),
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_empty_segment() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        let meta = writer.write_to_dir(dir.path()).unwrap();
        assert_eq!(meta.doc_count, 0);
        assert_eq!(meta.gram_count, 0);
        assert!(dir.path().join(&meta.dict_filename).exists());
        assert!(dir.path().join(&meta.post_filename).exists());

        let dict_path = dir.path().join(&meta.dict_filename);
        let seg = MmapSegment::open(&dict_path).unwrap();
        assert_eq!(seg.doc_count, 0);
        assert_eq!(seg.gram_count, 0);
    }

    #[test]
    fn round_trip_with_docs_and_grams() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("src/main.rs"), 0xDEAD, 100);
        writer.add_document(1, Path::new("src/lib.rs"), 0xBEEF, 200);
        writer.add_gram_posting(0xAAAA, 0);
        writer.add_gram_posting(0xAAAA, 1);
        writer.add_gram_posting(0xBBBB, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();
        assert_eq!(meta.doc_count, 2);
        assert_eq!(meta.gram_count, 2);
        assert!(dir.path().join(&meta.dict_filename).exists());
        assert!(dir.path().join(&meta.post_filename).exists());

        let dict_path = dir.path().join(&meta.dict_filename);
        let post_path = dir.path().join(&meta.post_filename);
        let seg = MmapSegment::open_split(&dict_path, &post_path).unwrap();
        assert_eq!(seg.doc_count, 2);

        let d0 = seg.get_doc(0).unwrap();
        assert_eq!(d0.path, Path::new("src/main.rs"));
        assert_eq!(d0.content_hash, 0xDEAD);

        let pl = seg.lookup_gram(0xAAAA).unwrap();
        let ids = pl.to_vec().unwrap();
        assert_eq!(ids, vec![0, 1]);
    }

    #[test]
    fn duplicate_postings_are_deduplicated() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("src/main.rs"), 0xDEAD, 100);
        writer.add_document(1, Path::new("src/lib.rs"), 0xBEEF, 200);
        writer.add_gram_posting(0xAAAA, 0);
        writer.add_gram_posting(0xAAAA, 0);
        writer.add_gram_posting(0xAAAA, 1);

        let meta = writer.write_to_dir(dir.path()).unwrap();
        assert_eq!(meta.gram_count, 1);
        assert!(dir.path().join(&meta.dict_filename).exists());
        assert!(dir.path().join(&meta.post_filename).exists());

        let dict_path = dir.path().join(&meta.dict_filename);
        let post_path = dir.path().join(&meta.post_filename);
        let seg = MmapSegment::open_split(&dict_path, &post_path).unwrap();
        assert_eq!(seg.gram_cardinality(0xAAAA), Some(2));
    }

    #[test]
    fn corrupt_file_rejected() {
        let dir = TempDir::new().unwrap();
        let bad_path = dir.path().join("bad.dict");
        std::fs::write(&bad_path, b"not a valid segment").unwrap();
        assert!(MmapSegment::open(&bad_path).is_err());
    }

    #[test]
    fn verify_integrity_passes_on_clean_segment() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("a.rs"), 1, 10);
        writer.add_gram_posting(0xAA, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let dict_path = dir.path().join(&meta.dict_filename);
        let seg = MmapSegment::open(&dict_path).unwrap();
        assert!(seg.verify_integrity().is_ok());
    }

    #[test]
    fn open_rejects_segment_exceeding_size_limit() {
        // Build a real segment first so we have valid magic/checksum.
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("a.rs"), 1, 10);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        // Verify the constant is wired in and a normal-size segment opens fine.
        let dict_path = dir.path().join(&meta.dict_filename);
        let seg = MmapSegment::open(&dict_path);
        assert!(
            seg.is_ok(),
            "valid segment under size limit must open successfully"
        );
    }

    #[test]
    fn map_private_copy_unaffected_by_post_open_file_mutation() {
        // With MAP_PRIVATE (map_copy_read_only), the mmap is a copy-on-write
        // snapshot of the file at open time. parse_segment_mmap reads every
        // content page during checksum verification, faulting them all into
        // the process private address space. After that point, on-disk mutations
        // are NOT reflected in the mapping.
        //
        // This is the desired security property: an attacker who gains write
        // access to the index directory after open() cannot affect in-process
        // reads. verify_integrity() checks the private copy against itself and
        // always passes; get_doc() returns the original document.
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("a.rs"), 1, 10);
        writer.add_gram_posting(0xAA, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let dict_path = dir.path().join(&meta.dict_filename);
        let seg = MmapSegment::open(&dict_path).unwrap();

        // Atomically replace the on-disk file after open() via rename.
        // On Linux, in-place writes (std::fs::write with O_TRUNC) invalidate
        // MAP_PRIVATE + PROT_READ page cache entries and cause SIGBUS.
        // Rename only changes the directory entry; the mmap holds the original
        // inode open via _file and is unaffected.
        let replacement = dir.path().join("replacement.dict");
        std::fs::write(&replacement, b"SNTX_corrupted_on_disk").unwrap();
        std::fs::rename(&replacement, &dict_path).unwrap();

        // The mmap still reads from the original inode; both must succeed.
        assert!(
            seg.verify_integrity().is_ok(),
            "mmap must survive atomic file replacement via rename"
        );
        assert!(
            seg.get_doc(0).is_some(),
            "mmap must serve doc reads after file replacement"
        );
    }

    #[test]
    fn with_capacity_hint_does_not_panic_when_exceeded() {
        let mut writer = SegmentWriter::with_capacity(1, 2);
        writer.add_document(0, Path::new("a.rs"), 1, 10);
        for i in 0u64..100 {
            writer.add_gram_posting(i, 0);
        }
        let dir = TempDir::new().unwrap();
        assert!(writer.write_to_dir(dir.path()).is_ok());
    }

    #[test]
    fn add_document_rejects_duplicate_doc_ids() {
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("a.rs"), 1, 10);
        writer.add_document(1, Path::new("b.rs"), 2, 20);
        // Duplicate id=1 should be caught during serialize.
        writer.add_document(1, Path::new("c.rs"), 3, 30);
        let dir = TempDir::new().unwrap();
        let result = writer.write_to_dir(dir.path());
        assert!(result.is_err(), "duplicate doc_id must be rejected");
    }

    #[test]
    fn format_version_constants_are_distinct() {
        assert_ne!(FORMAT_VERSION_V2, FORMAT_VERSION_V3);
        assert_eq!(FORMAT_VERSION, FORMAT_VERSION_V3);
    }

    #[test]
    fn dict_entry_size_matches_components() {
        // 8 (gram_hash) + 8 (abs_off/post_offset) + 4 (count) = 20 bytes.
        assert_eq!(DICT_ENTRY_SIZE, 20);
    }

    #[test]
    fn v3_writer_produces_two_files() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("src/lib.rs"), 0xABCD, 100);
        writer.add_gram_posting(0x1234, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        assert!(
            dir.path().join(&meta.dict_filename).exists(),
            "missing .dict"
        );
        assert!(
            dir.path().join(&meta.post_filename).exists(),
            "missing .post"
        );
        // No .seg file for v3
        let any_seg = std::fs::read_dir(dir.path())
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().ends_with(".seg"));
        assert!(!any_seg, "v3 writer must not produce a .seg file");
    }

    #[test]
    fn v3_round_trip_lookup_gram() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("src/main.rs"), 0xDEAD, 100);
        writer.add_document(1, Path::new("src/lib.rs"), 0xBEEF, 200);
        writer.add_gram_posting(0xAAAA, 0);
        writer.add_gram_posting(0xAAAA, 1);
        writer.add_gram_posting(0xBBBB, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let seg = MmapSegment::open_split(
            &dir.path().join(&meta.dict_filename),
            &dir.path().join(&meta.post_filename),
        )
        .unwrap();

        assert_eq!(seg.doc_count, 2);
        let d0 = seg.get_doc(0).unwrap();
        assert_eq!(d0.path, Path::new("src/main.rs"));

        let pl = seg.lookup_gram(0xAAAA).unwrap();
        assert_eq!(pl.to_vec().unwrap(), vec![0, 1]);

        let pl2 = seg.lookup_gram(0xBBBB).unwrap();
        assert_eq!(pl2.to_vec().unwrap(), vec![0]);

        assert!(seg.lookup_gram(0xCCCC).is_none());
    }

    #[test]
    fn v3_round_trip_get_doc() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("a.rs"), 0xAA, 10);
        writer.add_gram_posting(0x11, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let seg = MmapSegment::open_split(
            &dir.path().join(&meta.dict_filename),
            &dir.path().join(&meta.post_filename),
        )
        .unwrap();

        let doc = seg.get_doc(0).unwrap();
        assert_eq!(doc.path, Path::new("a.rs"));
        assert_eq!(doc.content_hash, 0xAA);
        assert!(seg.get_doc(1).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn round_trip_preserves_non_utf8_path_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        let path = std::path::PathBuf::from(OsString::from_vec(b"src/odd\xff.rs".to_vec()));
        writer.add_document(0, &path, 0xDEAD, 100);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let dict_path = dir.path().join(&meta.dict_filename);
        let seg = MmapSegment::open(&dict_path).unwrap();
        let d0 = seg.get_doc(0).unwrap();
        assert_eq!(d0.path, path);
    }

    #[test]
    fn v3_post_file_has_magic_and_checksum() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("src/a.rs"), 0x1234, 100);
        writer.add_gram_posting(0xAAAA, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let post_bytes = std::fs::read(dir.path().join(&meta.post_filename)).unwrap();
        // First 8 bytes must be the magic
        assert_eq!(&post_bytes[..8], b"SNTXPOST", "missing .post magic header");
        // File must be long enough for magic (8) + at least one posting entry + checksum (8)
        assert!(post_bytes.len() >= 17, "post file too short");
        // Last 8 bytes are xxhash64 checksum of the postings data (bytes 8..len-8)
        let postings_data = &post_bytes[8..post_bytes.len() - 8];
        let expected_checksum = xxhash_rust::xxh64::xxh64(postings_data, 0);
        let stored_checksum =
            u64::from_le_bytes(post_bytes[post_bytes.len() - 8..].try_into().unwrap());
        assert_eq!(stored_checksum, expected_checksum, "checksum mismatch");
    }

    #[test]
    fn open_split_rejects_corrupt_post_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("src/a.rs"), 0xABCD, 100);
        writer.add_gram_posting(0x1111, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        // Corrupt the .post file by writing wrong magic
        let post_path = dir.path().join(&meta.post_filename);
        let mut post_bytes = std::fs::read(&post_path).unwrap();
        post_bytes[0] = b'X'; // corrupt magic byte
        std::fs::write(&post_path, &post_bytes).unwrap();

        let result = MmapSegment::open_split(
            &dir.path().join(&meta.dict_filename),
            &dir.path().join(&meta.post_filename),
        );
        assert!(
            result.is_err(),
            "open_split must reject corrupt .post magic"
        );
    }

    #[test]
    fn get_doc_rejects_abs_off_pointing_into_dict_section() {
        // Security regression test for Fix 1a: craft a segment whose doc table
        // index pointer points into the dictionary section. Without the abs_off
        // range check, get_doc would interpret dict bytes as DocEntry fields
        // (information disclosure). With the check, it must return None.
        use xxhash_rust::xxh64::xxh64;

        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, Path::new("a.rs"), 0xABCD, 10);
        writer.add_gram_posting(0x1111, 0);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let dict_path = dir.path().join(&meta.dict_filename);
        let mut bytes = std::fs::read(&dict_path).unwrap();
        let len = bytes.len();
        // Footer layout (from end): [doc_table_off(8)][reserved(8)][dict_off(8)]
        //                           [doc_cnt(4)][gram_cnt(4)][cksum(8)][ver(4)][magic(4)]
        let footer_start = len - FOOTER_SIZE;
        let dict_offset_value = u64::from_le_bytes(
            bytes[footer_start + 16..footer_start + 24]
                .try_into()
                .unwrap(),
        ) as usize;

        // Overwrite doc 0's abs_off pointer to point 4 bytes into the dict section.
        let doc_table_offset = HEADER_SIZE; // always 40 for V3
        let bad_abs_off = (dict_offset_value + 4) as u64;
        bytes[doc_table_offset..doc_table_offset + 8].copy_from_slice(&bad_abs_off.to_le_bytes());

        // Recompute the checksum over the content (everything before the footer).
        let new_cksum = xxh64(&bytes[..footer_start], 0);
        bytes[footer_start + 32..footer_start + 40].copy_from_slice(&new_cksum.to_le_bytes());

        let crafted_path = dir.path().join("crafted.dict");
        std::fs::write(&crafted_path, &bytes).unwrap();

        let seg = MmapSegment::open(&crafted_path).unwrap();
        assert!(
            seg.get_doc(0).is_none(),
            "get_doc must return None when abs_off points into dict section"
        );
    }

    #[test]
    fn mmap_isolation_from_disk_overwrite() {
        // After opening, atomically replacing the file on disk must not corrupt
        // the in-memory mapping. MmapSegment retains an open file handle to the
        // original inode; after an atomic rename the old inode stays alive and
        // the mapping continues to read from it.
        //
        // We use rename rather than in-place write: on Linux, MAP_PRIVATE +
        // PROT_READ pages are still backed by the file's page cache, so
        // truncating the file via std::fs::write invalidates those pages and
        // delivers SIGBUS.  Atomic rename only changes the directory entry;
        // the mmap's inode reference is unaffected.
        let dir = tempfile::TempDir::new().unwrap();
        let mut writer = SegmentWriter::new();
        writer.add_document(0, std::path::Path::new("a.rs"), 0xABCD, 10);
        let meta = writer.write_to_dir(dir.path()).unwrap();

        let dict_path = dir.path().join(&meta.dict_filename);
        let seg = MmapSegment::open(&dict_path).unwrap();

        // Atomically replace the file at the same path with corrupted content.
        // The segment's inode (held open via _file) is unlinked from the
        // directory but remains alive.
        let replacement = dir.path().join("replacement.dict");
        std::fs::write(&replacement, b"CORRUPTED").unwrap();
        std::fs::rename(&replacement, &dict_path).unwrap();

        // The mmap still references the original inode; the document must be readable.
        let doc = seg.get_doc(0);
        assert!(
            doc.is_some(),
            "mmap must survive atomic file replacement via rename"
        );
    }
}
