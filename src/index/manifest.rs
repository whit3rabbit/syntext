//! Manifest: tracks active segments and overlay state.
//!
//! Persisted as `manifest.json` using atomic write-then-rename.
//! Reads on startup load segment metadata; GC removes orphan files.

// io::Error::new(ErrorKind::Other, ...) is used instead of io::Error::other()
// for Rust < 1.74 compatibility (Windows CI constraint).
#![allow(clippy::io_other_error)]

use std::io;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::index::segment::SegmentMeta;
use crate::IndexError;

/// Reference to an active base segment stored in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRef {
    /// UUID string that uniquely identifies this segment across rebuilds.
    pub segment_id: String,
    /// First global doc_id stored in this segment, if explicitly recorded.
    #[serde(default)]
    pub base_doc_id: Option<u32>,
    /// Legacy combined filename (`<uuid>.seg`). Empty for v3 segments.
    #[serde(default)]
    pub filename: String,
    /// Dictionary filename (`<uuid>.dict`) for v3 segments. Empty for v2.
    #[serde(default)]
    pub dict_filename: String,
    /// Postings filename (`<uuid>.post`) for v3 segments. Empty for v2.
    #[serde(default)]
    pub post_filename: String,
    /// Number of documents (files) indexed in this segment.
    pub doc_count: u32,
    /// Number of distinct n-gram hashes stored in this segment's dictionary.
    pub gram_count: u32,
}

impl From<SegmentMeta> for SegmentRef {
    fn from(m: SegmentMeta) -> Self {
        SegmentRef {
            segment_id: m.segment_id.to_string(),
            base_doc_id: None,
            filename: m.filename,
            dict_filename: m.dict_filename,
            post_filename: m.post_filename,
            doc_count: m.doc_count,
            gram_count: m.gram_count,
        }
    }
}

/// On-disk manifest describing the current index state.
///
/// Persisted as `manifest.json` via atomic write-then-rename so readers never
/// see a partially-written file. The `opstamp` field is reserved for
/// ordering concurrent writers in future phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Format version; must equal `FORMAT_VERSION` (currently 1).
    pub version: u32,
    /// Git commit SHA the base segments were built from, if recorded.
    pub base_commit: Option<String>,
    /// All active base segments; GC removes any `.seg` files not listed here.
    pub segments: Vec<SegmentRef>,
    /// Generation counter for overlay crash recovery. Reserved for future use;
    /// always written as 0 by the current implementation. On-disk generation
    /// files described in research.md §12 are not yet written (deferred to a
    /// later milestone). Do not remove: older manifest files include this field
    /// and removing it would silently corrupt them during deserialization.
    pub overlay_gen: u64,
    /// Filename of the on-disk overlay gram index, if present.
    pub overlay_file: Option<String>,
    /// Filename of the on-disk overlay deletes list, if present.
    pub overlay_deletes_file: Option<String>,
    /// Total files successfully indexed across all segments.
    pub total_files_indexed: u32,
    /// Unix timestamp (seconds) when this manifest was first created.
    pub created_at: u64,
    /// Operation stamp for future concurrent-write ordering (currently 0).
    pub opstamp: u64,
    /// Calibrated index vs. full-scan crossover fraction (0.0–1.0).
    /// `None` means this index was built without calibration; callers should
    /// fall back to the compile-time default of 0.10.
    #[serde(default)]
    pub scan_threshold_fraction: Option<f64>,
    /// xxh64 checksum of this manifest serialized with this field omitted.
    /// `None` means the manifest predates checksum support.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checksum: Option<u64>,
}

impl Manifest {
    const FILENAME: &'static str = "manifest.json";
    const FORMAT_VERSION: u32 = 1;

    /// Create a new manifest from a list of completed segments.
    pub fn new(segments: Vec<SegmentRef>, total_files_indexed: u32) -> Self {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Manifest {
            version: Self::FORMAT_VERSION,
            base_commit: None,
            segments,
            overlay_gen: 0,
            overlay_file: None,
            overlay_deletes_file: None,
            total_files_indexed,
            created_at,
            opstamp: 0,
            scan_threshold_fraction: None, // populated by Index::build() after calibration
            checksum: None,
        }
    }

    /// Maximum manifest file size (10 MB). A normal manifest for a 100K-file
    /// repo is under 1 MB. Anything larger is likely corrupt or adversarial.
    const MAX_MANIFEST_SIZE: u64 = 10 * 1024 * 1024;

    /// Load the manifest from `index_dir/manifest.json`.
    ///
    /// Security audit (deserialization): `serde_json::from_str` performs pure data
    /// parsing with no code execution, no gadget chains, and no polymorphic type
    /// resolution. The 10 MB size cap (`MAX_MANIFEST_SIZE`) bounds memory before
    /// parsing begins. Post-parse, `MAX_TOTAL_DOCS` (50M, in `index/mod.rs`)
    /// caps the doc count derived from segment refs, preventing unbounded
    /// allocations from a crafted manifest with inflated `doc_count` values.
    pub fn load(index_dir: &Path) -> Result<Self, IndexError> {
        let path = index_dir.join(Self::FILENAME);
        let meta = std::fs::metadata(&path)?;
        if meta.len() > Self::MAX_MANIFEST_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "manifest too large ({} bytes, max {})",
                    meta.len(),
                    Self::MAX_MANIFEST_SIZE
                ),
            )
            .into());
        }
        let data = std::fs::read_to_string(&path)?;
        let manifest: Self = serde_json::from_str(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        if let Some(stored_checksum) = manifest.checksum {
            let mut canonical_manifest = manifest.clone();
            canonical_manifest.checksum = None;
            let canonical_json =
                serde_json::to_string(&canonical_manifest).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            let computed_checksum = xxhash_rust::xxh64::xxh64(canonical_json.as_bytes(), 0);
            if computed_checksum != stored_checksum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "manifest checksum mismatch: stored {stored_checksum:#x}, computed {computed_checksum:#x}"
                    ),
                )
                .into());
            }
        } else {
            eprintln!(
                "syntext: warning: manifest at '{}' has no checksum; \
                 re-run `st index` to add one",
                path.display(),
            );
        }

        // Validate that each segment_id is a well-formed UUID. The field is not
        // currently used in filesystem paths, but future code (GC, logging,
        // metrics) could join it to a path. Reject non-UUID values proactively.
        for seg in &manifest.segments {
            if uuid::Uuid::parse_str(&seg.segment_id).is_err() {
                return Err(IndexError::CorruptIndex(format!(
                    "segment_id is not a valid UUID: {:?}",
                    seg.segment_id
                )));
            }
        }

        Ok(manifest)
    }

    /// Atomically persist the manifest to `index_dir/manifest.json`.
    pub fn save(&self, index_dir: &Path) -> io::Result<()> {
        // Use a random UUID for the temporary file to prevent TOCTOU (Time-of-Check to Time-of-Use)
        // symlink attacks where an attacker could pre-create `manifest.json.tmp` as a symlink
        // leading to arbitrary file overwrite.
        let tmp = index_dir.join(format!("manifest-{}.tmp", uuid::Uuid::new_v4()));
        let final_path = index_dir.join(Self::FILENAME);
        let mut canonical_manifest = self.clone();
        canonical_manifest.checksum = None;
        let canonical_json =
            serde_json::to_string(&canonical_manifest).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let checksum = xxhash_rust::xxh64::xxh64(canonical_json.as_bytes(), 0);

        let mut persisted_manifest = self.clone();
        persisted_manifest.checksum = Some(checksum);
        let json = serde_json::to_string_pretty(&persisted_manifest).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &final_path)?;
        #[cfg(not(windows))]
        std::fs::File::open(index_dir)?.sync_all()?;
        Ok(())
    }

    /// Remove segment files that exist on disk but are not referenced by this
    /// manifest. Called during build and after compaction to clean up orphans.
    ///
    /// **Unix safety:** `MmapSegment` holds an open `File` handle (`_file` field).
    /// On Linux and macOS, `unlink(2)` removes the directory entry but the inode
    /// (and mmap) remain valid until all file descriptors are closed. A reader
    /// that opened a segment before GC runs will continue to operate correctly.
    ///
    /// **Windows caveat:** Windows does not allow deleting a file that is open
    /// by another process/handle. `fs::remove_file` will return an error for
    /// any segment still mmap'd by an open `Index`. This is a known v1 limitation.
    pub fn gc_orphan_segments(&self, index_dir: &Path) -> io::Result<()> {
        let mut known: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for s in &self.segments {
            if !s.filename.is_empty() {
                known.insert(s.filename.as_str());
            }
            if !s.dict_filename.is_empty() {
                known.insert(s.dict_filename.as_str());
            }
            if !s.post_filename.is_empty() {
                known.insert(s.post_filename.as_str());
            }
        }
        for entry in std::fs::read_dir(index_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let is_segment_file = name_str.ends_with(".seg")
                || name_str.ends_with(".dict")
                || name_str.ends_with(".post");
            if is_segment_file && !known.contains(name_str.as_ref()) {
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    eprintln!("syntext: gc: could not remove {}: {e}", name_str);
                }
            }
            if name_str.ends_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// Total number of documents across all segments.
    pub fn total_docs(&self) -> u32 {
        self.segments.iter().map(|s| s.doc_count).sum()
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
