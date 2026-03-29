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
) -> Option<(std::path::PathBuf, Arc<[u8]>)> {
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
    // Use max_file_size + 1 as the read sentinel (same pattern as commit_batch).
    // If more than max_file_size bytes were read, the file grew since indexing;
    // skip it rather than silently verify only the truncated portion.
    let mut reader = file.take(max_file_size.saturating_add(1));
    let mut raw = Vec::new();
    reader.read_to_end(&mut raw).ok()?;
    if raw.len() as u64 > max_file_size {
        return None;
    }
    let content = crate::index::normalize_encoding(&raw);
    Some((doc_entry.path, Arc::from(content.as_ref())))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    #[test]
    fn oversized_file_returns_none() {
        let max: u64 = 16;
        // Write 17 bytes (> max).
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"12345678901234567").unwrap(); // 17 bytes
        tmp.flush().unwrap();

        // Re-open for reading (simulate what resolve_doc does).
        let file = std::fs::File::open(tmp.path()).unwrap();
        let mut reader = file.take(max.saturating_add(1));
        let mut content = Vec::new();
        reader.read_to_end(&mut content).unwrap();
        // Should read 17 bytes (all of them, since take(17) reads up to 17).
        assert!(content.len() as u64 > max, "must detect oversized file");
    }

    #[test]
    fn at_limit_file_is_not_skipped() {
        let max: u64 = 16;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"1234567890123456").unwrap(); // exactly 16 bytes
        tmp.flush().unwrap();

        let file = std::fs::File::open(tmp.path()).unwrap();
        let mut reader = file.take(max.saturating_add(1));
        let mut content = Vec::new();
        reader.read_to_end(&mut content).unwrap();
        assert!(
            content.len() as u64 <= max,
            "at-limit file must not be skipped"
        );
    }
}
