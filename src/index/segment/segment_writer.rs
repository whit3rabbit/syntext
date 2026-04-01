//! SNTX segment writer.
//!
//! Accumulates documents and gram postings in memory, then serializes them
//! to the SNTX v3 split format: a `.dict` file (header, doc table, page-aligned
//! dictionary, footer) and a `.post` file (raw postings bytes).
//! See `segment.rs` for format constants and shared types (`DocEntry`, `SegmentMeta`).

use std::io;
use std::path::Path;

use roaring::RoaringBitmap;
use uuid::Uuid;
use xxhash_rust::xxh64::xxh64;

use super::{DocEntry, SegmentMeta, FORMAT_VERSION, HEADER_SIZE, MAGIC, PAGE_SIZE};
use crate::path_util::path_bytes;
use crate::posting::{roaring_util, varint_encode, ROARING_THRESHOLD};

// ---------------------------------------------------------------------------
// T021: SegmentWriter
// ---------------------------------------------------------------------------

/// Accumulates documents and gram postings, then serializes to SNTX v3 split
/// files (`.dict` + `.post`).
pub struct SegmentWriter {
    docs: Vec<DocEntry>,
    /// Unsorted `(gram_hash, doc_id)` pairs, aggregated at write time.
    postings: Vec<(u64, u32)>,
    /// Capacity hint recorded at construction for debug overshoot detection.
    /// Only used in debug assertions; allowed to be dead in release builds.
    #[allow(dead_code)]
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
    pub fn add_document(&mut self, doc_id: u32, path: &Path, content_hash: u64, size_bytes: u64) {
        self.docs.push(DocEntry {
            doc_id,
            content_hash,
            size_bytes,
            path: path.to_path_buf(),
        });
    }

    /// Add a gram posting for a given document.
    pub fn add_gram_posting(&mut self, gram_hash: u64, doc_id: u32) {
        self.postings.push((gram_hash, doc_id));
    }

    /// Write segment into `dir`, naming files `{uuid}.dict` and `{uuid}.post`.
    ///
    /// Returns metadata whose `dict_filename` and `post_filename` match the
    /// files created on disk. Use this in production code so the manifest is
    /// always consistent.
    pub fn write_to_dir(mut self, dir: &Path) -> io::Result<SegmentMeta> {
        let segment_id = Uuid::new_v4();
        let dict_filename = format!("{}.dict", segment_id);
        let post_filename = format!("{}.post", segment_id);
        let (dict_bytes, post_bytes, doc_count, gram_count) = self.serialize()?;
        std::fs::write(dir.join(&dict_filename), &dict_bytes)?;
        std::fs::write(dir.join(&post_filename), &post_bytes)?;
        Ok(SegmentMeta {
            segment_id,
            filename: String::new(),
            dict_filename,
            post_filename,
            doc_count,
            gram_count,
        })
    }

    /// Write segment files derived from `path` (used in unit tests).
    ///
    /// Writes `{stem}.dict` and `{stem}.post` alongside the provided path,
    /// replacing whatever extension was given. The `SegmentMeta.dict_filename`
    /// and `post_filename` reflect the actual names created on disk.
    pub fn write_to_file(mut self, path: &Path) -> io::Result<SegmentMeta> {
        let segment_id = Uuid::new_v4();
        let parent = path.parent().unwrap_or(Path::new("."));
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("segment");
        let dict_path = parent.join(format!("{stem}.dict"));
        let post_path = parent.join(format!("{stem}.post"));
        let dict_filename = dict_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("segment.dict")
            .to_owned();
        let post_filename = post_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("segment.post")
            .to_owned();
        let (dict_bytes, post_bytes, doc_count, gram_count) = self.serialize()?;
        std::fs::write(&dict_path, &dict_bytes)?;
        std::fs::write(&post_path, &post_bytes)?;
        Ok(SegmentMeta {
            segment_id,
            filename: String::new(),
            dict_filename,
            post_filename,
            doc_count,
            gram_count,
        })
    }

    /// Build the on-disk byte representations.
    ///
    /// Sorts and deduplicates `self.docs` and `self.postings` in place, so
    /// `write_to_dir` / `write_to_file` consume `self` to prevent reuse.
    ///
    /// Returns `(dict_bytes, post_bytes, doc_count, gram_count)`.
    ///
    /// `dict_bytes`: header + doc table + page-aligned dictionary + footer.
    /// `post_bytes`: raw posting list data; offsets in dict entries are byte
    ///   offsets from the start of `post_bytes` (offset 0 = first byte).
    fn serialize(&mut self) -> io::Result<(Vec<u8>, Vec<u8>, u32, u32)> {
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
                "syntext: debug: SegmentWriter postings overshoot: hint={}, actual={}",
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
        buf.extend_from_slice(&0u64.to_le_bytes()); // postings_offset: 0 (reserved in v3; postings are in .post file)
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
            let path_bytes = path_bytes(&doc.path);
            let pb = path_bytes.as_ref();
            // Security: reject paths that exceed the u16 length prefix to
            // prevent silent truncation causing wrong-file attribution.
            let path_len = u16::try_from(pb.len()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "path exceeds u16::MAX bytes ({}): {}",
                        pb.len(),
                        doc.path.display()
                    ),
                )
            })?;
            buf.extend_from_slice(&path_len.to_le_bytes());
            buf.extend_from_slice(pb);
        }
        for (i, &abs_off) in doc_abs_offsets.iter().enumerate() {
            let p = idx_base + i * 8;
            buf[p..p + 8].copy_from_slice(&abs_off.to_le_bytes());
        }

        // Postings Section -- written to post_buf, not to the main dict buf.
        // Offsets stored in dict entries are byte offsets from the start of post_buf.
        let mut post_buf: Vec<u8> = Vec::new();
        let mut dict_entries: Vec<(u64, u64, u32)> = Vec::new();
        let mut posting_idx = 0usize;
        while posting_idx < self.postings.len() {
            let gram_hash = self.postings[posting_idx].0;
            let group_start = posting_idx;
            posting_idx += 1;
            while posting_idx < self.postings.len() && self.postings[posting_idx].0 == gram_hash {
                posting_idx += 1;
            }

            let posting_abs_off = post_buf.len() as u64; // offset within .post file
            let doc_ids: Vec<u32> = self.postings[group_start..posting_idx]
                .iter()
                .map(|(_, doc_id)| *doc_id)
                .collect();
            let entry_count = doc_ids.len() as u32;
            if doc_ids.len() >= ROARING_THRESHOLD {
                let bm: RoaringBitmap = doc_ids.iter().copied().collect();
                let rbytes = roaring_util::serialize(&bm);
                post_buf.push(1u8);
                post_buf.extend_from_slice(&entry_count.to_le_bytes());
                post_buf.extend_from_slice(&(rbytes.len() as u32).to_le_bytes());
                post_buf.extend_from_slice(&rbytes);
            } else {
                let encoded = varint_encode(&doc_ids).map_err(|msg| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("segment postings for gram {gram_hash:#x}: {msg}"),
                    )
                })?;
                post_buf.push(0u8);
                post_buf.extend_from_slice(&entry_count.to_le_bytes());
                post_buf.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
                post_buf.extend_from_slice(&encoded);
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
        buf[hdr_offsets_pos + 8..hdr_offsets_pos + 16].copy_from_slice(&0u64.to_le_bytes()); // postings_offset: 0 (reserved in v3; postings are in .post file)
        buf[hdr_offsets_pos + 16..hdr_offsets_pos + 24].copy_from_slice(&dict_offset.to_le_bytes());

        // TOC Footer
        let checksum = xxh64(&buf, 0);
        buf.extend_from_slice(&doc_table_offset.to_le_bytes()); // -48
        buf.extend_from_slice(&0u64.to_le_bytes()); // postings_offset: 0 (reserved in v3; postings are in .post file)
        buf.extend_from_slice(&dict_offset.to_le_bytes()); // -32
        buf.extend_from_slice(&doc_count.to_le_bytes()); // -24
        buf.extend_from_slice(&gram_count.to_le_bytes()); // -20
        buf.extend_from_slice(&checksum.to_le_bytes()); // -16
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes()); // -8
        buf.extend_from_slice(MAGIC); // -4

        // Wrap post_buf with magic header and xxhash64 checksum.
        // Layout: [b"SNTXPOST"][postings data][xxh64 checksum (8 bytes)]
        // Offsets in dict entries are relative to start of postings data (byte 8 in file).
        let post_checksum = xxh64(&post_buf, 0);
        let mut post_file_bytes = Vec::with_capacity(8 + post_buf.len() + 8);
        post_file_bytes.extend_from_slice(b"SNTXPOST");
        post_file_bytes.extend_from_slice(&post_buf);
        post_file_bytes.extend_from_slice(&post_checksum.to_le_bytes());

        Ok((buf, post_file_bytes, doc_count, gram_count))
    }
}
