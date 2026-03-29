//! Index statistics computation.

use crate::index::manifest::Manifest;
use crate::index::IndexSnapshot;
use crate::{Config, IndexStats};

/// Compute index statistics from the current snapshot and config.
pub fn compute_stats(snap: &IndexSnapshot, config: &Config, pending_edits: usize) -> IndexStats {
    let total_docs: usize = snap
        .base
        .segments
        .iter()
        .map(|s| s.doc_count as usize)
        .sum();
    let total_grams: usize = snap
        .base
        .segments
        .iter()
        .map(|s| s.gram_count as usize)
        .sum();
    let manifest_size = config
        .index_dir
        .join("manifest.json")
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0);
    let (seg_size, base_commit, overlay_generations) =
        if let Ok(manifest) = Manifest::load(&config.index_dir) {
            let seg_size = manifest
                .segments
                .iter()
                .map(|sr| {
                    // v3: sum .dict + .post; v2: just .seg
                    let dict_size = if !sr.dict_filename.is_empty() {
                        config
                            .index_dir
                            .join(&sr.dict_filename)
                            .metadata()
                            .map(|m| m.len())
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let post_size = if !sr.post_filename.is_empty() {
                        config
                            .index_dir
                            .join(&sr.post_filename)
                            .metadata()
                            .map(|m| m.len())
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let seg_file_size = if !sr.filename.is_empty() {
                        config
                            .index_dir
                            .join(&sr.filename)
                            .metadata()
                            .map(|m| m.len())
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    dict_size + post_size + seg_file_size
                })
                .sum();
            (
                seg_size,
                manifest.base_commit,
                manifest.overlay_gen as usize,
            )
        } else {
            (0, None, 0)
        };

    IndexStats {
        total_documents: total_docs,
        total_segments: snap.base.segments.len(),
        total_grams,
        index_size_bytes: manifest_size + seg_size,
        base_commit,
        overlay_generations,
        pending_edits,
    }
}
