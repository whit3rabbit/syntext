//! The write path: compile in bounded batches, stage segment files, commit
//! one manifest.
//!
//! A write may produce several segments. Documents are compiled and written
//! out [`StoreOptions::batch_bytes`] of source text at a time, so memory
//! stays proportional to one batch however large the write (a first sync of
//! a large repository, a full compaction). Nothing becomes visible until the
//! commit, which publishes every staged segment, marks superseded documents
//! dead, and drops replaced segments in a single manifest rename. Segment
//! files staged by a write that fails before its commit are orphans that
//! the next commit or open collects.
//!
//! [`StoreOptions::batch_bytes`]: super::StoreOptions::batch_bytes

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::files;
use super::manifest::{segment_file_name, unix_now, SegmentEntry};
use super::writer::write_segment;
use super::{assemble, ContextStore, IngestReport, Writer};
use crate::compile::{compile_all, SkipReason, SourceDoc};
use crate::error::Result;
use crate::types::DocKey;

impl ContextStore {
    /// Compile and publish `docs`, replacing documents with the same keys.
    pub fn upsert(&self, docs: Vec<SourceDoc>) -> Result<IngestReport> {
        self.apply(docs, Vec::new())
    }

    /// Remove documents by key. Unknown keys are ignored.
    pub fn remove(&self, keys: &[DocKey]) -> Result<IngestReport> {
        self.apply(Vec::new(), keys.to_vec())
    }

    /// Upsert and remove in one atomic generation. When a key appears more
    /// than once in `upserts`, the last occurrence wins. A document that
    /// fails to compile (binary, oversized) also removes its previous
    /// version, so a file that turned binary does not linger as text.
    pub fn apply(&self, upserts: Vec<SourceDoc>, removals: Vec<DocKey>) -> Result<IngestReport> {
        let mut seen = HashSet::new();
        let mut unique: Vec<SourceDoc> = upserts
            .into_iter()
            .rev()
            .filter(|d| seen.insert(d.key.clone()))
            .collect();
        unique.reverse();
        self.write_batched(|staged| {
            for doc in unique {
                staged.push(doc)?;
            }
            Ok(removals)
        })
    }

    /// Run `fill` under the writer lock, staging the documents it pushes,
    /// then commit them together with the removals it returns.
    pub(crate) fn write_batched(
        &self,
        fill: impl FnOnce(&mut StagedWrite<'_>) -> Result<Vec<DocKey>>,
    ) -> Result<IngestReport> {
        let mut w = self.writer()?;
        let mut staged = StagedWrite::new(self, &mut w);
        let removals = fill(&mut staged)?;
        staged.commit(removals, None, &[])
    }
}

/// One staged segment file and the keys of its documents, in local order.
struct Staged {
    entry: SegmentEntry,
    keys: Vec<DocKey>,
}

/// A write in progress: documents compiled and written to segment files
/// batch by batch, published by [`commit`](Self::commit).
pub(crate) struct StagedWrite<'a> {
    store: &'a ContextStore,
    w: &'a mut Writer,
    generation: u64,
    pending: Vec<SourceDoc>,
    pending_bytes: usize,
    staged: Vec<Staged>,
    skipped: Vec<(DocKey, SkipReason)>,
    passages: usize,
}

impl<'a> StagedWrite<'a> {
    pub(super) fn new(store: &'a ContextStore, w: &'a mut Writer) -> Self {
        let generation = w.manifest.generation + 1;
        StagedWrite {
            store,
            w,
            generation,
            pending: Vec::new(),
            pending_bytes: 0,
            staged: Vec::new(),
            skipped: Vec::new(),
            passages: 0,
        }
    }

    /// Add a document; a full batch is compiled and written immediately.
    pub(crate) fn push(&mut self, doc: SourceDoc) -> Result<()> {
        self.pending_bytes += doc.text.len();
        self.pending.push(doc);
        if self.pending_bytes >= self.store.options.batch_bytes.max(1) {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.pending_bytes = 0;
        let (compiled, skipped) =
            compile_all(std::mem::take(&mut self.pending), &self.w.manifest.compile);
        self.skipped.extend(skipped);
        if compiled.is_empty() {
            return Ok(());
        }
        self.passages += compiled.iter().map(|d| d.passages.len()).sum::<usize>();
        let (bytes, meta) = write_segment(&compiled, self.w.manifest.compile.graph, unix_now())?;
        let name = segment_file_name(self.generation);
        files::write_atomic(&self.store.dir, &name, &bytes, self.store.options.sync)?;
        self.staged.push(Staged {
            entry: SegmentEntry {
                file: name,
                generation: self.generation,
                docs: meta.docs,
                passages: meta.passages,
                bytes: bytes.len() as u64,
                dead: Vec::new(),
            },
            keys: compiled.into_iter().map(|d| d.meta.key).collect(),
        });
        Ok(())
    }

    /// Publish everything staged: mark superseded and `removals` documents
    /// dead, drop the `drop` segments, advance `memory_through`, and commit
    /// a new manifest. Nothing after the commit point can fail except
    /// best-effort garbage collection. A write with nothing to publish
    /// leaves the generation unchanged.
    pub(super) fn commit(
        mut self,
        mut removals: Vec<DocKey>,
        memory_through: Option<u64>,
        drop: &[String],
    ) -> Result<IngestReport> {
        self.flush()?;
        let snap = self.store.snapshot();
        // Later stagings of a key supersede earlier ones in the same write.
        let mut latest: HashMap<&str, (usize, u32)> = HashMap::new();
        let mut staged_dead: Vec<Vec<u32>> = vec![Vec::new(); self.staged.len()];
        for (si, s) in self.staged.iter().enumerate() {
            for (di, key) in s.keys.iter().enumerate() {
                if let Some((ps, pd)) = latest.insert(key.as_str(), (si, di as u32)) {
                    staged_dead[ps].push(pd);
                }
            }
        }
        removals.extend(self.skipped.iter().map(|(k, _)| k.clone()));
        removals.sort();
        removals.dedup();
        removals.retain(|k| snap.lookup(k.as_str()).is_some() && !latest.contains_key(k.as_str()));
        let report = IngestReport {
            generation: snap.generation,
            upserted: latest.len(),
            removed: removals.len(),
            passages: self.passages,
            segments: self.staged.len(),
            skipped: std::mem::take(&mut self.skipped),
        };
        if self.staged.is_empty()
            && removals.is_empty()
            && drop.is_empty()
            && memory_through.is_none()
        {
            return Ok(report);
        }

        let mut manifest = self.w.manifest.clone();
        manifest.generation = self.generation;
        for key in latest
            .keys()
            .copied()
            .chain(removals.iter().map(DocKey::as_str))
        {
            if let Some(loc) = snap.lookup(key) {
                let h = &snap.segments[loc.seg as usize];
                if let Some(e) = manifest
                    .segments
                    .iter_mut()
                    .find(|e| h.durable && e.file == h.name)
                {
                    e.dead.push(loc.doc);
                }
            }
        }
        manifest.segments.retain(|e| !drop.contains(&e.file));
        for (s, dead) in self.staged.iter().zip(staged_dead) {
            manifest.segments.push(SegmentEntry {
                dead,
                ..s.entry.clone()
            });
        }
        for e in &mut manifest.segments {
            e.dead.sort_unstable();
            e.dead.dedup();
        }
        if let Some(m) = memory_through {
            manifest.memory_through = manifest.memory_through.max(m);
        }
        let store = self.store;
        let next = assemble(
            &store.dir,
            &store.options,
            &manifest,
            Some(&snap),
            self.w.live_handle.clone(),
        )?;
        manifest.save(&store.dir, store.options.sync)?;
        self.w.manifest = manifest;
        store.snapshot.store(Arc::new(next));
        files::gc(&store.dir, &self.w.manifest);
        Ok(IngestReport {
            generation: self.generation,
            ..report
        })
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
