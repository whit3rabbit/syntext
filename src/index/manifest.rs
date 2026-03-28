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
            scan_threshold_fraction: None,  // populated by Index::build() after calibration
        }
    }

    /// Maximum manifest file size (10 MB). A normal manifest for a 100K-file
    /// repo is under 1 MB. Anything larger is likely corrupt or adversarial.
    const MAX_MANIFEST_SIZE: u64 = 10 * 1024 * 1024;

    /// Load the manifest from `index_dir/manifest.json`.
    pub fn load(index_dir: &Path) -> io::Result<Self> {
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
            ));
        }
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
        }
        Ok(())
    }

    /// Total number of documents across all segments.
    pub fn total_docs(&self) -> u32 {
        self.segments.iter().map(|s| s.doc_count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_rejects_oversized_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(Manifest::FILENAME);
        let data = "x".repeat(11 * 1024 * 1024);
        std::fs::write(&path, data).unwrap();

        let result = Manifest::load(dir.path());
        assert!(result.is_err(), "should reject manifest > 10MB");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("too large"),
            "error should mention size: {err_msg}"
        );
    }

    #[test]
    fn load_accepts_normal_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest = Manifest::new(vec![], 0);
        manifest.save(dir.path()).unwrap();

        let loaded = Manifest::load(dir.path()).unwrap();
        assert_eq!(loaded.total_docs(), 0);
    }

    #[test]
    fn roundtrip_preserves_scan_threshold() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut manifest = Manifest::new(vec![], 0);
        manifest.scan_threshold_fraction = Some(0.23);
        manifest.save(dir.path()).unwrap();

        let loaded = Manifest::load(dir.path()).unwrap();
        assert_eq!(
            loaded.scan_threshold_fraction,
            Some(0.23),
            "scan_threshold_fraction must round-trip through manifest.json"
        );
    }

    #[test]
    fn missing_threshold_deserializes_as_none() {
        let dir = tempfile::TempDir::new().unwrap();
        // Write a manifest without the field (simulates old index).
        let json = r#"{
        "version": 1,
        "base_commit": null,
        "segments": [],
        "overlay_gen": 0,
        "overlay_file": null,
        "overlay_deletes_file": null,
        "total_files_indexed": 0,
        "created_at": 0,
        "opstamp": 0
    }"#;
        std::fs::write(dir.path().join("manifest.json"), json).unwrap();

        let loaded = Manifest::load(dir.path()).unwrap();
        assert!(
            loaded.scan_threshold_fraction.is_none(),
            "old manifests without the field must deserialize as None"
        );
    }

    #[test]
    fn segment_ref_round_trips_with_post_filename() {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_ref = SegmentRef {
            segment_id: "test-uuid".into(),
            filename: String::new(),
            dict_filename: "test-uuid.dict".into(),
            post_filename: "test-uuid.post".into(),
            doc_count: 5,
            gram_count: 10,
        };
        let manifest = Manifest::new(vec![seg_ref], 5);
        manifest.save(dir.path()).unwrap();
        let loaded = Manifest::load(dir.path()).unwrap();
        assert_eq!(loaded.segments[0].dict_filename, "test-uuid.dict");
        assert_eq!(loaded.segments[0].post_filename, "test-uuid.post");
    }

    #[test]
    fn gc_removes_orphan_dict_and_post_files() {
        let dir = tempfile::TempDir::new().unwrap();
        // Create orphaned .dict and .post files
        std::fs::write(dir.path().join("orphan.dict"), b"orphan").unwrap();
        std::fs::write(dir.path().join("orphan.post"), b"orphan").unwrap();
        // Also create referenced files
        std::fs::write(dir.path().join("kept.dict"), b"kept").unwrap();
        std::fs::write(dir.path().join("kept.post"), b"kept").unwrap();

        let manifest = Manifest::new(
            vec![SegmentRef {
                segment_id: "kept".into(),
                filename: String::new(),
                dict_filename: "kept.dict".into(),
                post_filename: "kept.post".into(),
                doc_count: 0,
                gram_count: 0,
            }],
            0,
        );
        manifest.gc_orphan_segments(dir.path()).unwrap();

        assert!(
            !dir.path().join("orphan.dict").exists(),
            "orphan .dict must be removed"
        );
        assert!(
            !dir.path().join("orphan.post").exists(),
            "orphan .post must be removed"
        );
        assert!(
            dir.path().join("kept.dict").exists(),
            "referenced .dict must be kept"
        );
        assert!(
            dir.path().join("kept.post").exists(),
            "referenced .post must be kept"
        );
    }

    #[test]
    fn v2_manifest_without_split_filenames_deserializes_cleanly() {
        let dir = tempfile::TempDir::new().unwrap();
        // Simulate a v2 manifest with no dict_filename / post_filename fields
        let json = r#"{
        "version": 1,
        "base_commit": null,
        "segments": [
            {
                "segment_id": "old-uuid",
                "filename": "old-uuid.seg",
                "doc_count": 10,
                "gram_count": 50
            }
        ],
        "overlay_gen": 0,
        "overlay_file": null,
        "overlay_deletes_file": null,
        "total_files_indexed": 10,
        "created_at": 0,
        "opstamp": 0
    }"#;
        std::fs::write(dir.path().join("manifest.json"), json).unwrap();
        let loaded = Manifest::load(dir.path()).unwrap();
        assert_eq!(loaded.segments[0].filename, "old-uuid.seg");
        assert_eq!(loaded.segments[0].dict_filename, ""); // defaults to empty
        assert_eq!(loaded.segments[0].post_filename, ""); // defaults to empty
    }
}
