//! Index statistics computation.

use crate::index::manifest::Manifest;
use crate::index::IndexSnapshot;
use crate::{Config, IndexStats};

/// Compute index statistics from the current snapshot and config.
pub fn compute_stats(snap: &IndexSnapshot, config: &Config) -> IndexStats {
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
    let seg_size: u64 = if let Ok(manifest) = Manifest::load(&config.index_dir) {
        manifest
            .segments
            .iter()
            .map(|sr| {
                config
                    .index_dir
                    .join(&sr.filename)
                    .metadata()
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum()
    } else {
        0
    };

    IndexStats {
        total_documents: total_docs,
        total_segments: snap.base.segments.len(),
        total_grams,
        index_size_bytes: manifest_size + seg_size,
        base_commit: None,
        overlay_generations: 0,
        pending_edits: 0,
    }
}
