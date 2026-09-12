//! Batch application of observed changes to the index: `Index::apply_change_batch`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::{commit::RequeueGuard, helpers, Index};
use crate::changes::{ChangeBatch, ChangeKind};
use crate::IndexError;

impl Index {
    /// Apply an observed [`ChangeBatch`] directly to the index without
    /// re-reading files whose content buffers were already pre-loaded.
    ///
    /// Changes are made visible to subsequent queries in the overlay.
    /// Returns the batch generation applied.
    pub fn apply_change_batch(&self, batch: &ChangeBatch) -> Result<u64, IndexError> {
        if batch.is_empty() {
            return Ok(batch.generation);
        }

        let mut preloaded: HashMap<PathBuf, Arc<[u8]>> = HashMap::new();
        let mut has_ops = false;

        // Buffer all operations into pending.
        for record in &batch.records {
            let rel = if record.path.is_relative() {
                crate::path_util::normalize_to_forward_slashes(record.path.clone())
            } else {
                self.repo_relative_path(&record.path)?
            };
            match record.kind {
                ChangeKind::Added | ChangeKind::Modified => {
                    has_ops = true;
                    if let Some(ref content) = record.content {
                        preloaded.insert(rel.clone(), Arc::clone(content));
                    }
                    self.pending.notify_change(&rel);
                }
                ChangeKind::Deleted => {
                    has_ops = true;
                    self.pending.notify_delete(&rel);
                }
                ChangeKind::Touched | ChangeKind::Unchanged | ChangeKind::Uncertain => {
                    // Touched / Unchanged do not modify index content.
                    // Uncertain paths require caller/watcher rescan.
                }
            }
        }

        if !has_ops {
            return Ok(batch.generation);
        }

        let _write_lock = helpers::acquire_writer_lock(&self.config.index_dir)?;

        let old_snap = self.snapshot.load_full();
        let take = self.pending.take_for_commit();
        let mut requeue_guard = RequeueGuard {
            pending: &self.pending,
            take: Some(take),
        };

        self.commit_inner_with_preloaded(
            &old_snap,
            requeue_guard.take.as_mut().expect("take present"),
            Some(&preloaded),
        )?;

        // Success: disarm the requeue guard so drained edits are retired.
        requeue_guard.take = None;
        Ok(batch.generation)
    }
}
