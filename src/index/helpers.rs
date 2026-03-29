//! Free-standing helper functions used by the index subsystem.

use std::path::Path;
use std::process::Command;

use crate::index::overlay::OverlayView;
use crate::index::snapshot::IndexSnapshot;
use crate::IndexError;

pub(super) fn resolve_git_binary() -> std::path::PathBuf {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("git");
            if candidate.is_file() {
                if let Ok(resolved) = candidate.canonicalize() {
                    return resolved;
                }
            }
        }
    }
    std::path::PathBuf::from("/usr/bin/git")
}

pub(super) fn current_repo_head(repo_root: &Path) -> Result<Option<String>, IndexError> {
    let canonical_root = std::fs::canonicalize(repo_root)?;
    let output = match Command::new(resolve_git_binary())
        .arg("-C")
        .arg(&canonical_root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return Ok(None),
    };

    if !output.status.success() {
        return Ok(None);
    }

    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if head.is_empty() {
        Ok(None)
    } else {
        Ok(Some(head))
    }
}

pub(super) fn acquire_writer_lock(index_dir: &Path) -> Result<std::fs::File, IndexError> {
    use fs2::FileExt;
    let write_lock_path = index_dir.join("write.lock");
    let write_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&write_lock_path)?;
    write_lock
        .try_lock_exclusive()
        .map_err(|_| IndexError::LockConflict(index_dir.to_path_buf()))?;
    Ok(write_lock)
}

pub(super) fn projected_overlay_doc_count(
    old_overlay: &OverlayView,
    visible_changed: &std::collections::HashSet<std::path::PathBuf>,
    removed_paths: &std::collections::HashSet<std::path::PathBuf>,
) -> usize {
    old_overlay
        .docs
        .iter()
        .filter(|doc| !visible_changed.contains(&doc.path) && !removed_paths.contains(&doc.path))
        .count()
        + visible_changed
            .iter()
            .filter(|p| !removed_paths.contains(*p))
            .count()
}

pub(super) fn base_doc_id_limit(snapshot: &IndexSnapshot) -> Result<u32, IndexError> {
    let mut max_limit: u32 = 0;
    for (seg_idx, seg) in snapshot.base_segments().iter().enumerate() {
        if let Some(&base) = snapshot.segment_base_ids().get(seg_idx) {
            let limit = base
                .checked_add(seg.doc_count)
                .ok_or(IndexError::DocIdOverflow {
                    base_doc_count: base,
                    overlay_docs: seg.doc_count as usize,
                })?;
            if limit > max_limit {
                max_limit = limit;
            }
        }
    }
    Ok(max_limit)
}
