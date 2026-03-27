//! Manifest: tracks active segments and overlay state.
//!
//! Persisted as `manifest.json` using atomic write-then-rename.
//! Reads on startup load segment metadata; GC removes orphan files.

use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::index::segment::SegmentMeta;

/// Reference to an active base segment stored in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRef {
    /// UUID string that uniquely identifies this segment across rebuilds.
    pub segment_id: String,
    /// Filename relative to the index directory (e.g., `"<uuid>.seg"`).
    pub filename: String,
    /// Number of documents (files) indexed in this segment.
    pub doc_count: u32,
    /// Number of distinct n-gram hashes stored in this segment's dictionary.
    pub gram_count: u32,
}

impl From<SegmentMeta> for SegmentRef {
    fn from(m: SegmentMeta) -> Self {
        SegmentRef {
            segment_id: m.segment_id.to_string(),
            filename: m.filename,
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
    /// Monotonically increasing counter incremented on each overlay commit.
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
        }
    }

    /// Load the manifest from `index_dir/manifest.json`.
    pub fn load(index_dir: &Path) -> io::Result<Self> {
        let path = index_dir.join(Self::FILENAME);
        let data = std::fs::read_to_string(&path)?;
        serde_json::from_str(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Atomically persist the manifest to `index_dir/manifest.json`.
    pub fn save(&self, index_dir: &Path) -> io::Result<()> {
        // Use a random UUID for the temporary file to prevent TOCTOU (Time-of-Check to Time-of-Use)
        // symlink attacks where an attacker could pre-create `manifest.json.tmp` as a symlink 
        // leading to arbitrary file overwrite.
        let tmp = index_dir.join(format!("manifest-{}.tmp", uuid::Uuid::new_v4()));
        let final_path = index_dir.join(Self::FILENAME);
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, final_path)?;
        Ok(())
    }

    /// Delete segment files in `index_dir` not referenced by this manifest.
    pub fn gc_orphan_segments(&self, index_dir: &Path) -> io::Result<()> {
        let known: std::collections::HashSet<&str> =
            self.segments.iter().map(|s| s.filename.as_str()).collect();
        for entry in std::fs::read_dir(index_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".seg") && !known.contains(name_str.as_ref()) {
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    eprintln!("ripline: gc: could not remove {}: {e}", name_str);
                }
            }
        }
        Ok(())
    }

    /// Total number of documents across all segments.
    pub fn total_docs(&self) -> u32 {
        self.segments.iter().map(|s| s.doc_count).sum()
    }
}
