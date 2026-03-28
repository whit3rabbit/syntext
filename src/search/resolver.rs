//! Document content resolver: maps global doc_id to path + content.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use crate::index::IndexSnapshot;

/// Resolve a global doc ID to its path and content.
/// Overlay docs return in-memory content. Base docs read from disk, capped at max_file_size.
/// Returns None if deleted, out of range, or unreadable.
pub(super) fn resolve_doc(
    snap: &IndexSnapshot,
    global_id: u32,
    canonical_root: &Path,
    max_file_size: u64,
) -> Option<(String, Arc<[u8]>)> {
    if let Some(doc) = snap.overlay.get_doc(global_id) {
        return Some((doc.path.clone(), Arc::clone(&doc.content)));
    }
    if snap.delete_set.contains(global_id) {
        return None;
    }
    if snap.segment_base_ids().is_empty() {
        return None;
    }
    let seg_idx = snap
        .segment_base_ids()
        .partition_point(|&b| b <= global_id)
        .saturating_sub(1);
    if seg_idx >= snap.base_segments().len() {
        return None;
    }
    let base = snap.segment_base_ids()[seg_idx];
    let local_id = global_id.checked_sub(base)?;
    let doc_entry = snap.base_segments()[seg_idx].get_doc(local_id)?;

    let abs_path = canonical_root.join(&doc_entry.path);
    let canonical = std::fs::canonicalize(&abs_path).ok()?;
    if !canonical.starts_with(canonical_root) {
        return None;
    }
    // Mitigate TOCTOU: stat before open, then verify fd matches the same inode.
    let pre_meta = std::fs::metadata(&canonical).ok()?;
    let file = crate::index::open_readonly_nofollow(&canonical).ok()?;
    #[cfg(unix)]
    if !crate::index::verify_fd_matches_stat(&file, &pre_meta) {
        return None;
    }
    let mut reader = file.take(max_file_size);
    let mut content = Vec::new();
    reader.read_to_end(&mut content).ok()?;
    Some((doc_entry.path, Arc::from(content)))
}
