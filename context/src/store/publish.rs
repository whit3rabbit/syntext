//! Snapshot publication for writable handles.
//!
//! A snapshot has two halves written by different paths: the durable
//! segments (segment writes and compaction, under the segment lock) and the
//! live conversation-memory segment (turn appends, under the memory lock).
//! [`Published`] keeps the inputs of the current snapshot; each path
//! replaces its own half and rebuilds under one short lock, so neither can
//! publish over the other's newer state.

use std::path::Path;
use std::sync::Arc;

use super::manifest::Manifest;
use super::reader::SegmentReader;
use super::snapshot::SegmentHandle;
use super::{lock, ContextStore, StoreOptions, StoreSnapshot, Writable};
use crate::error::{ContextError, Result};

/// The inputs of the published snapshot.
pub(super) struct Published {
    pub generation: u64,
    pub store_id: String,
    pub durable: Vec<Arc<SegmentHandle>>,
    /// Dead local document ids per durable segment, from the manifest.
    pub dead: Vec<Vec<u32>>,
    pub live: Option<Arc<SegmentHandle>>,
}

impl Published {
    /// Inputs for `manifest`, its opened `durable` segments, and `live`.
    pub(super) fn new(
        manifest: &Manifest,
        durable: Vec<Arc<SegmentHandle>>,
        live: Option<Arc<SegmentHandle>>,
    ) -> Self {
        Published {
            generation: manifest.generation,
            store_id: manifest.store_id.clone(),
            durable,
            dead: dead_lists(manifest),
            live,
        }
    }

    /// Build the snapshot these inputs describe.
    pub(super) fn snapshot(&self) -> StoreSnapshot {
        let mut segments = self.durable.clone();
        let live_through = self.live.as_ref().map_or(0, |h| {
            h.reader.docs.iter().map(|d| d.seq).max().unwrap_or(0)
        });
        if let Some(h) = &self.live {
            segments.push(Arc::clone(h));
        }
        let dead: Vec<&[u32]> = self.dead.iter().map(Vec::as_slice).collect();
        let mut snap =
            StoreSnapshot::build(self.generation, self.store_id.clone(), segments, &dead);
        snap.live_through = live_through;
        snap
    }
}

/// Dead lists of every segment `manifest` names, in order.
pub(super) fn dead_lists(manifest: &Manifest) -> Vec<Vec<u32>> {
    manifest.segments.iter().map(|e| e.dead.clone()).collect()
}

/// Open the durable segments `manifest` names, reusing handles already
/// open in `prev`.
pub(super) fn open_durable(
    dir: &Path,
    options: &StoreOptions,
    manifest: &Manifest,
    prev: Option<&StoreSnapshot>,
) -> Result<Vec<Arc<SegmentHandle>>> {
    let mut segments = Vec::with_capacity(manifest.segments.len());
    for e in &manifest.segments {
        let reused = prev.and_then(|p| {
            p.segments
                .iter()
                .find(|s| s.durable && s.name == e.file)
                .cloned()
        });
        let handle = match reused {
            Some(h) => h,
            None => Arc::new(SegmentHandle {
                name: e.file.clone(),
                durable: true,
                reader: SegmentReader::open_file(&dir.join(&e.file), options.verify_on_open)?,
            }),
        };
        if handle.reader.docs.len() != e.docs as usize {
            return Err(ContextError::corrupt(format!(
                "segment {} does not match the manifest",
                e.file
            )));
        }
        segments.push(handle);
    }
    Ok(segments)
}

impl ContextStore {
    /// Replace part of the published inputs and publish the rebuilt
    /// snapshot.
    pub(super) fn publish(&self, w: &Writable, update: impl FnOnce(&mut Published)) {
        let mut published = lock(&w.published);
        update(&mut published);
        self.snapshot.store(Arc::new(published.snapshot()));
    }
}
