//! Segment reader: open/mmap helpers and posting-list read routines.
//!
//! Extracted from `mod.rs` to keep non-test line count under 400.

#[cfg(feature = "memmap2")]
use std::path::Path;

#[cfg(feature = "memmap2")]
use memmap2::MmapOptions;
use xxhash_rust::xxh64::xxh64;

#[cfg(feature = "memmap2")]
use crate::index::segment::MAX_SEGMENT_SIZE;
use crate::index::segment::{
    MmapSegment, PostingsBacking, SegmentData, FOOTER_SIZE, FORMAT_VERSION_V2, FORMAT_VERSION_V3,
    HEADER_SIZE, MAGIC,
};
use crate::IndexError;

/// Parsed offsets and counts extracted from a segment mmap's footer.
pub(super) struct SegmentLayout {
    pub doc_table_offset: usize,
    pub dict_offset: usize,
    pub doc_count: u32,
    pub gram_count: u32,
    /// Conservative lower bound for postings data: past the fixed-size doc
    /// index entries (8 bytes each).
    pub postings_start: usize,
}

/// Validate magic, checksum, and parse offsets from a segment mmap.
///
/// `accepted_versions`: slice of version numbers this caller accepts.
/// Returns an error if the mmap is malformed or the version is not in the list.
pub(super) fn parse_segment_mmap(
    mmap: &[u8],
    accepted_versions: &[u32],
) -> Result<SegmentLayout, IndexError> {
    let len = mmap.len();
    let corrupt = |msg: &str| IndexError::CorruptIndex(msg.into());

    if len < HEADER_SIZE + FOOTER_SIZE {
        return Err(corrupt("file too small"));
    }

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
    if !accepted_versions.contains(&version) {
        return Err(IndexError::CorruptIndex(format!(
            "unsupported segment version {version}"
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

    // Security: validate that both offsets fall inside (or at the boundary of)
    // the content area. The checksum above proves content integrity but does NOT
    // constrain what values the footer's offset fields contain — a crafted segment
    // with a valid checksum could embed offsets pointing anywhere in the mmap.
    // Without this check, get_doc() and dict_lookup() would read attacker-chosen
    // mmap bytes as structured data, enabling information disclosure even though
    // safe Rust's .get() bounds checks prevent memory-safety violations.
    //
    // Valid range: [HEADER_SIZE, content_end] where content_end = len - FOOTER_SIZE.
    // Upper bound is inclusive (<=, not <) to allow empty sections: for a segment
    // with 0 documents or 0 grams the corresponding section has zero bytes and its
    // offset legally equals content_end (pointing just past the content area).
    // Reads at such offsets are safe because doc_count/gram_count = 0 means the
    // calling code never issues a .get() at that position.
    let content_end = len - FOOTER_SIZE;
    if doc_table_offset < HEADER_SIZE || doc_table_offset > content_end {
        return Err(corrupt("doc_table_offset out of range"));
    }
    if dict_offset < HEADER_SIZE || dict_offset > content_end {
        return Err(corrupt("dict_offset out of range"));
    }
    // dict must not overlap the doc table: a crafted segment with dict_offset ==
    // doc_table_offset would cause dict_lookup to binary-search doc table bytes as
    // dict entries, producing garbage posting offsets from an attacker-controlled region.
    if dict_offset < doc_table_offset {
        return Err(corrupt("dict_offset precedes doc_table_offset"));
    }

    let postings_start = doc_table_offset.saturating_add(doc_count as usize * 8);

    Ok(SegmentLayout {
        doc_table_offset,
        dict_offset,
        doc_count,
        gram_count,
        postings_start,
    })
}

/// Cross-platform positional read: reads exactly `buf.len()` bytes from `file`
/// starting at `offset` without changing the file cursor.
#[cfg(feature = "memmap2")]
pub(super) fn read_exact_at(
    file: &std::fs::File,
    buf: &mut [u8],
    offset: u64,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(not(unix))]
    {
        use std::io::{Read, Seek, SeekFrom};
        // TODO: use seek_read on Windows (std::os::windows::fs::FileExt::seek_read)
        // for a truly cursor-free positional read. This clone-and-seek fallback is
        // acceptable for the current macOS/Linux target set.
        let mut owned = file.try_clone()?;
        owned.seek(SeekFrom::Start(offset))?;
        owned.read_exact(buf)
    }
}

/// Size of the `.post` file magic header in bytes.
/// Posting offsets stored in dict entries are relative to the start of postings
/// data (i.e., byte 0 of postings data = byte POST_MAGIC_SIZE of the file).
#[cfg(feature = "memmap2")]
pub(super) const POST_MAGIC_SIZE: u64 = 8;

/// Read a posting list from the `.post` file at byte offset `abs_off`.
///
/// `abs_off` is relative to the start of postings data (after the magic header).
/// This function adds `POST_MAGIC_SIZE` to convert to a file-absolute offset.
///
/// Security: `byte_len` is bounded to 64 MB before allocation to prevent OOM
/// from a malformed `.post` file.
#[cfg(feature = "memmap2")]
pub(super) fn read_posting_list_pread(
    post_file: &std::fs::File,
    abs_off: u64,
) -> std::io::Result<crate::posting::PostingList> {
    use std::io::{Error, ErrorKind};

    use crate::posting::{roaring_util, PostingList};

    // abs_off is relative to start of postings data (byte POST_MAGIC_SIZE of .post file).
    // Add POST_MAGIC_SIZE to convert to file-absolute offset.
    // Read the 9-byte entry header: encoding(1) + count(4) + byte_len(4).
    //
    // Use checked_add: abs_off comes from on-disk dict data and could be
    // crafted to wrap around u64::MAX, redirecting the read to byte 0 of the
    // .post file (the magic header bytes) and producing a silent false negative.
    let mut header = [0u8; 9];
    let header_off = abs_off.checked_add(POST_MAGIC_SIZE).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "posting header offset overflow",
        )
    })?;
    read_exact_at(post_file, &mut header, header_off)?;

    let encoding = header[0];
    // infallible: header is [u8; 9]; header[5..9] is always exactly 4 bytes
    let byte_len = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;

    const MAX_POSTING_BYTES: usize = 8 * 1024 * 1024;
    // Bounds check: prevent OOM from a malformed `.post` file.
    if byte_len > MAX_POSTING_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("posting list too large: {byte_len} bytes (max {MAX_POSTING_BYTES})"),
        ));
    }

    let mut data = vec![0u8; byte_len];
    let data_off = abs_off.checked_add(9 + POST_MAGIC_SIZE).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "posting data offset overflow",
        )
    })?;
    read_exact_at(post_file, &mut data, data_off)?;

    match encoding {
        0 => Ok(PostingList::Small(data)),
        1 => roaring_util::deserialize(&data)
            .map(PostingList::Large)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string())),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("unknown posting list encoding {encoding}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// MmapSegment open methods
// ---------------------------------------------------------------------------

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
        let layout = parse_segment_mmap(&*mmap, &[FORMAT_VERSION_V3])?;
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
