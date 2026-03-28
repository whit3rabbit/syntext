//! Posting-list read helpers and mmap parsing for v3 split-file segments.
//!
//! Extracted from `mod.rs` to keep non-test line count under 400.

use memmap2::Mmap;
use xxhash_rust::xxh64::xxh64;

use crate::index::segment::{FOOTER_SIZE, HEADER_SIZE, MAGIC};
use crate::posting::{roaring_util, PostingList};
use crate::IndexError;

/// Parsed offsets and counts extracted from a segment mmap's footer.
pub(super) struct SegmentLayout {
    pub doc_table_offset: usize,
    pub dict_offset: usize,
    pub doc_count: u32,
    pub gram_count: u32,
}

/// Validate magic, checksum, and parse offsets from a segment mmap.
///
/// `accepted_versions`: slice of version numbers this caller accepts.
/// Returns an error if the mmap is malformed or the version is not in the list.
pub(super) fn parse_segment_mmap(
    mmap: &Mmap,
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

    // Security: validate that both offsets fall inside the content area of the
    // file. The checksum above proves content integrity but does NOT constrain
    // what values the footer's offset fields contain — a crafted segment with a
    // valid checksum could embed offsets pointing anywhere in the mmap. Without
    // this check, get_doc() and dict_lookup() would read attacker-chosen mmap
    // bytes as structured data, enabling information disclosure even though safe
    // Rust's .get() bounds checks prevent memory-safety violations.
    //
    // Valid range for both offsets: [HEADER_SIZE, len - FOOTER_SIZE).
    // The doc table begins immediately after the 40-byte header; the dictionary
    // section follows the postings area. Both must fit inside the content region.
    let content_end = len - FOOTER_SIZE;
    if doc_table_offset < HEADER_SIZE || doc_table_offset >= content_end {
        return Err(corrupt("doc_table_offset out of range"));
    }
    if dict_offset < HEADER_SIZE || dict_offset >= content_end {
        return Err(corrupt("dict_offset out of range"));
    }

    Ok(SegmentLayout {
        doc_table_offset,
        dict_offset,
        doc_count,
        gram_count,
    })
}

/// Cross-platform positional read: reads exactly `buf.len()` bytes from `file`
/// starting at `offset` without changing the file cursor.
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
pub(super) const POST_MAGIC_SIZE: u64 = 8;

/// Read a posting list from the `.post` file at byte offset `abs_off`.
///
/// `abs_off` is relative to the start of postings data (after the magic header).
/// This function adds `POST_MAGIC_SIZE` to convert to a file-absolute offset.
///
/// Security: `byte_len` is bounded to 64 MB before allocation to prevent OOM
/// from a malformed `.post` file.
pub(super) fn read_posting_list_pread(
    post_file: &std::fs::File,
    abs_off: u64,
) -> std::io::Result<PostingList> {
    use std::io::{Error, ErrorKind};

    // abs_off is relative to start of postings data (byte POST_MAGIC_SIZE of .post file).
    // Add POST_MAGIC_SIZE to convert to file-absolute offset.
    // Read the 9-byte entry header: encoding(1) + count(4) + byte_len(4).
    let mut header = [0u8; 9];
    read_exact_at(post_file, &mut header, abs_off + POST_MAGIC_SIZE)?;

    let encoding = header[0];
    let byte_len = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;

    const MAX_POSTING_BYTES: usize = 64 * 1024 * 1024;
    // Bounds check: prevent OOM from a malformed `.post` file.
    if byte_len > MAX_POSTING_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("posting list too large: {byte_len} bytes (max {MAX_POSTING_BYTES})"),
        ));
    }

    let mut data = vec![0u8; byte_len];
    read_exact_at(post_file, &mut data, abs_off + 9 + POST_MAGIC_SIZE)?;

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
