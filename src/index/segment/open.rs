//! MmapSegment constructors: from_bytes, open (v2), open_split (v3).
#![allow(clippy::io_other_error)]

#[cfg(feature = "memmap2")]
use std::path::Path;

#[cfg(feature = "memmap2")]
use memmap2::MmapOptions;
#[cfg(feature = "memmap2")]
use xxhash_rust::xxh64::xxh64;

#[cfg(feature = "memmap2")]
use super::MAX_SEGMENT_SIZE;
use super::reader::parse_segment_mmap;
#[cfg(feature = "memmap2")]
use super::reader::read_exact_at;
use super::{
    MmapSegment, PostingsBacking, SegmentData, FORMAT_VERSION_V2, FORMAT_VERSION_V3,
};
use crate::IndexError;

impl MmapSegment {
    /// Load a segment entirely from in-memory bytes (WASM / tests).
    ///
    /// `dict_bytes`: the full `.dict` file content.
    /// `post_bytes`: the full `.post` file content (including SNTXPOST magic and checksum).
    /// No filesystem access, no mmap, no advisory locking.
    pub fn from_bytes(dict_bytes: Vec<u8>, post_bytes: Vec<u8>) -> Result<Self, IndexError> {
        let layout = parse_segment_mmap(&dict_bytes, &[FORMAT_VERSION_V2, FORMAT_VERSION_V3])?;
        let len = dict_bytes.len();
        Ok(MmapSegment {
            _file: None,
            expected_len: len,
            doc_count: layout.doc_count,
            gram_count: layout.gram_count,
            doc_table_offset: layout.doc_table_offset,
            dict_offset: layout.dict_offset,
            postings_start: layout.postings_start,
            mmap: SegmentData::Heap(dict_bytes),
            postings: PostingsBacking::InMemory(post_bytes),
        })
    }

    /// Open a combined (v2) segment file, verify magic, version, and checksum.
    #[cfg(feature = "memmap2")]
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
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
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
        let layout = parse_segment_mmap(&mmap, &[FORMAT_VERSION_V2, FORMAT_VERSION_V3])?;

        Ok(MmapSegment {
            _file: Some(file),
            mmap: SegmentData::Mmap(mmap),
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
    #[cfg(feature = "memmap2")]
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
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
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
        let layout = parse_segment_mmap(&mmap, &[FORMAT_VERSION_V3])?;
        let post_file = std::fs::File::open(post_path)?;
        post_file
            .try_lock_shared()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

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
        read_exact_at(&post_file, &mut post_magic, 0)?;
        if &post_magic != POST_MAGIC {
            return Err(IndexError::CorruptIndex(
                "post file has wrong magic (expected SNTXPOST)".into(),
            ));
        }

        // Read and verify the checksum (last 8 bytes cover the postings data
        // between the magic header and checksum trailer).
        let checksum_offset = (post_len - 8) as u64;
        let mut stored_cksum_bytes = [0u8; 8];
        read_exact_at(&post_file, &mut stored_cksum_bytes, checksum_offset)?;
        let stored_post_checksum = u64::from_le_bytes(stored_cksum_bytes);

        // Read postings data (bytes 8..post_len-8) to compute expected checksum.
        let postings_data_len = post_len - 16; // subtract magic(8) + checksum(8)
        let mut postings_data = vec![0u8; postings_data_len];
        if postings_data_len > 0 {
            read_exact_at(&post_file, &mut postings_data, 8)?;
        }
        let expected_post_checksum = xxh64(&postings_data, 0);
        if stored_post_checksum != expected_post_checksum {
            return Err(IndexError::CorruptIndex(
                "post file checksum mismatch".into(),
            ));
        }

        Ok(MmapSegment {
            _file: Some(file),
            mmap: SegmentData::Mmap(mmap),
            expected_len: len,
            doc_count: layout.doc_count,
            gram_count: layout.gram_count,
            doc_table_offset: layout.doc_table_offset,
            dict_offset: layout.dict_offset,
            postings_start: 0,
            postings: PostingsBacking::V3File(post_file),
        })
    }
}
