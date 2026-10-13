//! Filesystem helpers: atomic writes, the writer lock, and garbage
//! collection of superseded files.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use super::manifest::{is_segment_file_name, sync_dir, Manifest};
use crate::embed::table::sidecar_segment;
use crate::error::{ContextError, Result};

pub(crate) const LOCK_FILE: &str = "write.lock";

/// Write `bytes` to `dir/name` atomically: temp file, sync, rename, sync
/// directory (syncs skipped when `sync` is false).
pub(crate) fn write_atomic(dir: &Path, name: &str, bytes: &[u8], sync: bool) -> Result<()> {
    let tmp = dir.join(format!("{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        if sync {
            f.sync_all()?;
        }
    }
    if let Err(e) = std::fs::rename(&tmp, dir.join(name)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if sync {
        sync_dir(dir)?;
    }
    Ok(())
}

/// Take the store's exclusive writer lock, held for the handle's lifetime.
///
/// `WouldBlock` means another writer holds it. `EINTR` is retried; any
/// other lock failure (e.g. `ENOLCK` under heavy process churn) is reported
/// as [`ContextError::Locked`] too, with the errno logged, because the
/// caller's remedy (back off and retry) is the same.
pub(crate) fn acquire_lock(dir: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))?;
    for _ in 0..8 {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => break,
            Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Interrupted => {
            }
            Err(std::fs::TryLockError::Error(e)) => {
                log::warn!("context store lock on {} failed: {e}", dir.display());
                break;
            }
        }
    }
    Err(ContextError::Locked(dir.to_path_buf()))
}

/// Remove segment files the manifest no longer references, the vector
/// sidecars of those segments, and temp files left by crashed writers. Only called while holding the writer lock, so
/// no other process can be mid-write. Best effort: a file still open
/// elsewhere on Windows cannot be deleted and is retried next time.
pub(crate) fn gc(dir: &Path, manifest: &Manifest) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let orphan_segment =
            is_segment_file_name(&name) && !manifest.segments.iter().any(|s| s.file == name);
        let orphan_vectors = sidecar_segment(&name)
            .is_some_and(|seg| !manifest.segments.iter().any(|s| s.file == seg));
        if orphan_segment || orphan_vectors || name.ends_with(".tmp") {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                log::debug!("context store gc: could not remove {name}: {e}");
            }
        }
    }
}
