//! Store manifest: the live segment list and the commit point of every write.
//!
//! A write becomes visible exactly when a new `manifest.json` is renamed into
//! place. Segment files are written and synced first, so a crash at any point
//! leaves either the old manifest (new segment is an orphan, swept later) or
//! the new one (everything it names is durable). The manifest carries an
//! xxh64 checksum of its canonical JSON, so a torn or hand-edited file is
//! detected instead of silently misread.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh64::xxh64;

use crate::compile::CompileOptions;
use crate::error::{ContextError, Result};

pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const FORMAT_NAME: &str = "syntext-context";
pub(crate) const MANIFEST_VERSION: u32 = 1;

/// One durable segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SegmentEntry {
    pub file: String,
    pub generation: u64,
    pub docs: u32,
    pub passages: u32,
    pub bytes: u64,
    /// Local document ids superseded by later segments or removed, sorted.
    #[serde(default)]
    pub dead: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: String,
    pub version: u32,
    pub store_id: String,
    pub generation: u64,
    pub created_unix: i64,
    pub updated_unix: i64,
    pub compile: CompileOptions,
    pub segments: Vec<SegmentEntry>,
    /// Highest memory-journal sequence that is durably indexed in a segment.
    #[serde(default)]
    pub memory_through: u64,
    #[serde(default)]
    pub checksum: Option<u64>,
}

impl Manifest {
    pub(crate) fn new(compile: CompileOptions) -> Self {
        let now = unix_now();
        Manifest {
            format: FORMAT_NAME.to_string(),
            version: MANIFEST_VERSION,
            store_id: uuid::Uuid::new_v4().to_string(),
            generation: 0,
            created_unix: now,
            updated_unix: now,
            compile,
            segments: Vec::new(),
            memory_through: 0,
            checksum: None,
        }
    }

    /// Load and validate `dir/manifest.json`; `Ok(None)` when absent.
    pub(crate) fn load(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join(MANIFEST_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let m: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| ContextError::corrupt(format!("manifest: {e}")))?;
        if m.format != FORMAT_NAME {
            return Err(ContextError::corrupt("not a syntext-context manifest"));
        }
        if m.version != MANIFEST_VERSION {
            return Err(ContextError::UnsupportedVersion {
                found: m.version,
                supported: MANIFEST_VERSION,
            });
        }
        let stored = m
            .checksum
            .ok_or_else(|| ContextError::corrupt("manifest has no checksum"))?;
        if stored != m.canonical_checksum()? {
            return Err(ContextError::corrupt("manifest checksum mismatch"));
        }
        for s in &m.segments {
            let dead_ok =
                s.dead.windows(2).all(|w| w[0] < w[1]) && s.dead.last().is_none_or(|&d| d < s.docs);
            if !is_segment_file_name(&s.file) || !dead_ok {
                return Err(ContextError::corrupt(format!(
                    "bad segment entry {:?}",
                    s.file
                )));
            }
        }
        Ok(Some(m))
    }

    fn canonical_checksum(&self) -> Result<u64> {
        let mut canonical = self.clone();
        canonical.checksum = None;
        let json =
            serde_json::to_vec(&canonical).map_err(|e| ContextError::invalid(e.to_string()))?;
        Ok(xxh64(&json, 0))
    }

    /// Atomically replace `dir/manifest.json` (write temp, sync, rename,
    /// sync directory). With `sync == false` the fsyncs are skipped.
    pub(crate) fn save(&mut self, dir: &Path, sync: bool) -> Result<()> {
        self.updated_unix = unix_now();
        self.checksum = Some(self.canonical_checksum()?);
        let json =
            serde_json::to_vec_pretty(&*self).map_err(|e| ContextError::invalid(e.to_string()))?;
        // A random temp name: a pre-planted symlink at a fixed name cannot
        // redirect the write.
        let tmp = dir.join(format!("manifest-{}.tmp", uuid::Uuid::new_v4()));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(&json)?;
            if sync {
                f.sync_all()?;
            }
        }
        if let Err(e) = std::fs::rename(&tmp, dir.join(MANIFEST_FILE)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        if sync {
            sync_dir(dir)?;
        }
        Ok(())
    }
}

/// Flush a directory entry update (rename/create). Directory handles cannot
/// be synced on Windows, where the rename is already durable enough.
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(not(windows))]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(windows)]
    let _ = dir;
    Ok(())
}

/// Segment files are named `seg-<generation>-<id>.ctxs`; nothing else (no
/// separators, no `..`) is accepted from a manifest.
pub(crate) fn is_segment_file_name(name: &str) -> bool {
    name.len() <= 96
        && name.starts_with("seg-")
        && name.ends_with(".ctxs")
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

pub(crate) fn segment_file_name(generation: u64) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("seg-{generation:012}-{}.ctxs", &id[..12])
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_round_trip_with_checksum() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Manifest::load(dir.path()).unwrap().is_none());
        let mut m = Manifest::new(CompileOptions::default());
        m.segments.push(SegmentEntry {
            file: segment_file_name(1),
            generation: 1,
            docs: 3,
            passages: 9,
            bytes: 100,
            dead: vec![0, 2],
        });
        m.save(dir.path(), true).unwrap();
        let back = Manifest::load(dir.path()).unwrap().unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn tampering_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manifest::new(CompileOptions::default());
        m.save(dir.path(), false).unwrap();
        let path = dir.path().join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace("\"generation\": 0", "\"generation\": 7"),
        )
        .unwrap();
        assert!(matches!(
            Manifest::load(dir.path()),
            Err(ContextError::Corrupt(_))
        ));
    }

    #[test]
    fn segment_names_are_validated() {
        assert!(is_segment_file_name(&segment_file_name(42)));
        for bad in [
            "../seg-1.ctxs",
            "seg-1.ctxs/x",
            "seg-a/../b.ctxs",
            "manifest.json",
            "SEG-1.ctxs",
        ] {
            assert!(!is_segment_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_dead_lists_and_versions() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manifest::new(CompileOptions::default());
        m.segments.push(SegmentEntry {
            file: segment_file_name(1),
            generation: 1,
            docs: 2,
            passages: 2,
            bytes: 1,
            dead: vec![5],
        });
        m.save(dir.path(), false).unwrap();
        assert!(Manifest::load(dir.path()).is_err());
        let mut m = Manifest::new(CompileOptions::default());
        m.version = 99;
        m.save(dir.path(), false).unwrap();
        assert!(matches!(
            Manifest::load(dir.path()),
            Err(ContextError::UnsupportedVersion { found: 99, .. })
        ));
    }
}
