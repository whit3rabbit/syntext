//! Working-tree file reads for a full build.
//!
//! Split from `build.rs` to keep it under the 400-line quality gate. A build
//! with a content source never calls this; see `index::source`.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Read one enumerated file from the working tree for a build, or `None` (and
/// count it unreadable) when it cannot be read safely.
pub(super) fn read_working_tree_file(
    abs_path: &Path,
    skipped_unreadable: &AtomicUsize,
) -> Option<Vec<u8>> {
    // Security: close the TOCTOU window between enumerate_files()'s
    // symlink resolution (symlink_metadata + canonicalize) and this
    // read. open_readonly_nofollow blocks final-component substitution;
    // verify_fd_matches_stat catches directory-component swaps.
    let pre_meta = match std::fs::symlink_metadata(abs_path) {
        Ok(m) => m,
        Err(e) => {
            log::debug!("skipping {}: stat: {e}", abs_path.display());
            skipped_unreadable.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    let mut file = match super::open_readonly_nofollow(abs_path) {
        Ok(f) => f,
        Err(e) => {
            log::debug!("skipping {}: open: {e}", abs_path.display());
            skipped_unreadable.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    #[cfg(any(unix, windows))]
    if !super::verify_fd_matches_stat(&file, &pre_meta) {
        skipped_unreadable.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    #[cfg(not(any(unix, windows)))]
    let _ = &pre_meta;
    let mut raw = Vec::new();
    if let Err(e) = file.read_to_end(&mut raw) {
        log::debug!("skipping {}: read: {e}", abs_path.display());
        skipped_unreadable.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some(raw)
}
