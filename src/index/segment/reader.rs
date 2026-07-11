//! Segment reader: open/mmap helpers and posting-list read routines.
//!
//! Extracted from `mod.rs` to keep non-test line count under 400.

use xxhash_rust::xxh64::xxh64;

use crate::index::segment::{DictVerify, FOOTER_SIZE, HEADER_SIZE, MAGIC};
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
/// `dict_verify` selects how much of `mmap` is checksummed: `Full` reads the
/// entire content region and verifies the stored xxh64 trailer (as before);
/// `Structural` skips that O(content length) pass and relies solely on the
/// O(1) checks below (magic, version, footer presence, offsets in range).
/// See [`DictVerify`] for the security argument behind skipping the full pass.
/// Returns an error if the mmap is malformed or the version is not in the list.
pub(super) fn parse_segment_mmap(
    mmap: &[u8],
    accepted_versions: &[u32],
    dict_verify: DictVerify,
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
    // O(content length) pass: only run it when the caller asked for Full
    // verification. Structural callers (ordinary open) skip this and rely on
    // the O(1) checks in this function instead; postings and dict entries are
    // still read from real file/mmap bytes at query time, so skipping this
    // does not open a fabrication path (see DictVerify's doc comment).
    if dict_verify == DictVerify::Full {
        let content = mmap
            .get(..len - FOOTER_SIZE)
            .ok_or_else(|| corrupt("truncated: cannot read content"))?;
        if xxh64(content, 0) != stored_checksum {
            return Err(corrupt("checksum mismatch"));
        }
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
    #[cfg(windows)]
    {
        // `seek_read` issues a single `ReadFile` with the offset in an
        // `OVERLAPPED` struct: no handle dup, no separate `SetFilePointerEx`.
        // It is not strictly pread-equivalent (on a synchronous handle the file
        // pointer is updated as a side effect), but every caller here supplies
        // its own offset and never reads from the current position, so the
        // trailing cursor value is unobserved. Loop to handle short reads.
        use std::io::{Error, ErrorKind};
        use std::os::windows::fs::FileExt;
        let mut remaining = buf;
        let mut off = offset;
        while !remaining.is_empty() {
            match file.seek_read(remaining, off) {
                Ok(0) => {
                    return Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "failed to fill buffer",
                    ));
                }
                Ok(n) => {
                    remaining = &mut remaining[n..];
                    off += n as u64;
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Fallback for exotic targets with neither Unix pread nor Windows
        // seek_read. NOTE: unlike the pread/seek_read paths, this does NOT
        // guarantee `file`'s cursor is preserved: `try_clone` may `dup` the fd
        // (a shared open-file-description offset) on POSIX-like targets, so
        // seeking the clone can move the original's cursor. It is correct only
        // because this crate reads segments purely positionally and never
        // relies on the cursor between calls. This path is unexercised today
        // (wasm32 bypasses all file I/O); a real port here should switch to a
        // save/restore-seek or a positional syscall rather than trust the
        // clone to be offset-independent.
        use std::io::{Read, Seek, SeekFrom};
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
/// Security: `byte_len` is bounded to 8 MB before allocation to prevent OOM
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

pub(super) const POST_MAGIC: &[u8; 8] = b"SNTXPOST";
pub(super) const POST_MIN_SIZE: usize = 8 + 8;

/// O(1) structural validation of a `.post` file: minimum size, magic header,
/// and a readable checksum trailer. Three positional reads, no allocation.
#[cfg(feature = "memmap2")]
pub(super) fn check_post_file_structure(post_file: &std::fs::File) -> Result<(), IndexError> {
    let post_len = post_file.metadata()?.len() as usize;
    if post_len < POST_MIN_SIZE {
        return Err(IndexError::CorruptIndex(format!(
            "post file too small: {post_len} bytes"
        )));
    }
    let mut post_magic = [0u8; 8];
    read_exact_at(post_file, &mut post_magic, 0)?;
    if &post_magic != POST_MAGIC {
        return Err(IndexError::CorruptIndex(
            "post file has wrong magic (expected SNTXPOST)".into(),
        ));
    }
    // Trailer presence only: the stored value is compared against a recomputed
    // checksum exclusively in the Full pass (verify_post_file_checksum).
    let mut trailer = [0u8; 8];
    read_exact_at(post_file, &mut trailer, (post_len - 8) as u64)?;
    Ok(())
}

/// Stream the `.post` file in chunks and verify its xxh64 trailer checksum.
/// O(post file size) I/O with O(1) heap allocation (a fixed read buffer),
/// avoiding the transient multi-hundred-MB allocation the previous
/// read-the-whole-file approach needed for large segments.
#[cfg(feature = "memmap2")]
pub(super) fn verify_post_file_checksum(post_file: &std::fs::File) -> Result<(), IndexError> {
    use xxhash_rust::xxh64::Xxh64;

    let post_len = post_file.metadata()?.len() as usize;
    if post_len < POST_MIN_SIZE {
        return Err(IndexError::CorruptIndex(format!(
            "post file too small: {post_len} bytes"
        )));
    }
    let mut stored_cksum_bytes = [0u8; 8];
    read_exact_at(post_file, &mut stored_cksum_bytes, (post_len - 8) as u64)?;
    let stored_post_checksum = u64::from_le_bytes(stored_cksum_bytes);

    // Postings data lies between the magic header (8 bytes) and the checksum
    // trailer (8 bytes). Stream it through the incremental xxh64 hasher in
    // fixed-size chunks instead of allocating the whole postings region.
    let postings_data_len = post_len - 16;
    let mut hasher = Xxh64::new(0);
    // 64 KB balances syscall count against cache friendliness; a full segment
    // checksum pass over a ~100 MB .post file is ~1600 preads at this size.
    let mut buf = vec![0u8; 64 * 1024];
    let mut offset: u64 = 8;
    let mut remaining = postings_data_len;
    while remaining > 0 {
        let take = remaining.min(buf.len());
        let chunk = &mut buf[..take];
        read_exact_at(post_file, chunk, offset)?;
        hasher.update(chunk);
        offset += take as u64;
        remaining -= take;
    }
    if hasher.digest() != stored_post_checksum {
        return Err(IndexError::CorruptIndex(
            "post file checksum mismatch".into(),
        ));
    }
    Ok(())
}

