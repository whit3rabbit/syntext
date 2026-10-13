//! Compaction: merge segments and physically drop dead documents.
//!
//! Every write adds segments and leaves superseded documents behind as dead
//! entries. Compaction recompiles the live documents of a set of segments
//! from their stored text and metadata into new segments (in bounded
//! batches, like any write) and drops the old ones. Recompiling keeps the
//! writer the single source of the format and is also the upgrade path: a
//! segment compiled by an older [`COMPILER_VERSION`] is always due.
//!
//! Merging is tiered so a large store is not rewritten for every small
//! write: when small segments (under about half a batch) pile up, only they
//! are merged. The whole store is rewritten only when too much of it is
//! dead or a segment is stale.

use serde::Serialize;
use syntext::changes::ContentDigest;

use super::write::StagedWrite;
use super::ContextStore;
use crate::compile::{SourceDoc, COMPILER_VERSION};
use crate::error::Result;
use crate::types::{DocKey, Scope};

/// Outcome of a compaction.
#[derive(Debug, Clone, Serialize)]
pub struct CompactReport {
    /// Generation after compaction (unchanged when nothing was done).
    pub generation: u64,
    /// Durable segments before.
    pub segments_before: usize,
    /// Durable segments after.
    pub segments_after: usize,
    /// Live documents rewritten.
    pub documents: usize,
    /// Dead documents physically removed.
    pub dead_dropped: u64,
}

impl ContextStore {
    /// Rewrite every durable segment, dropping dead documents.
    pub fn compact(&self) -> Result<CompactReport> {
        self.rewrite(|_, _| true)
    }

    /// Compact when needed: everything when more than `max_dead_ratio` of
    /// documents are dead or a segment was compiled by an older
    /// [`COMPILER_VERSION`]; only the small segments when there are more
    /// than `max_segments` of them.
    pub fn maybe_compact(&self) -> Result<Option<CompactReport>> {
        let snap = self.snapshot();
        let mut docs = 0u64;
        let mut dead = 0u64;
        let mut stale = false;
        let mut small = 0usize;
        for (i, s) in snap.segments.iter().enumerate() {
            if !s.durable {
                continue;
            }
            stale |= s.reader.meta.compiler != COMPILER_VERSION;
            small += usize::from(self.is_small(s.reader.byte_len()));
            let n = s.reader.docs.len() as u32;
            docs += u64::from(n);
            dead += (0..n).filter(|&d| !snap.doc_live(i as u32, d)).count() as u64;
        }
        let ratio = if docs == 0 {
            0.0
        } else {
            dead as f32 / docs as f32
        };
        if stale || ratio > self.options.max_dead_ratio {
            self.compact().map(Some)
        } else if small > self.options.max_segments {
            self.rewrite(|this, bytes| this.is_small(bytes)).map(Some)
        } else {
            Ok(None)
        }
    }

    /// A segment file under the batch size holds less than about half a
    /// batch of source text.
    fn is_small(&self, segment_bytes: u64) -> bool {
        segment_bytes < self.options.batch_bytes as u64
    }

    /// Recompile the live documents of the durable segments `select`
    /// accepts (by file size) into new segments and drop the old ones.
    fn rewrite(&self, select: impl Fn(&Self, u64) -> bool) -> Result<CompactReport> {
        let mut w = self.writer()?;
        let snap = self.snapshot();
        let durable: Vec<usize> = (0..snap.segments.len())
            .filter(|&i| snap.segments[i].durable)
            .collect();
        let chosen: Vec<usize> = durable
            .iter()
            .copied()
            .filter(|&i| select(self, snap.segments[i].reader.byte_len()))
            .collect();
        let view = &snap;
        let live = |i: usize| {
            let n = view.segments[i].reader.docs.len() as u32;
            (0..n).filter(move |&d| view.doc_live(i as u32, d))
        };
        let dead: u64 = chosen
            .iter()
            .map(|&i| snap.segments[i].reader.docs.len() as u64 - live(i).count() as u64)
            .sum();
        let stale = chosen
            .iter()
            .any(|&i| snap.segments[i].reader.meta.compiler != COMPILER_VERSION);
        let mut report = CompactReport {
            generation: snap.generation,
            segments_before: durable.len(),
            segments_after: durable.len(),
            documents: 0,
            dead_dropped: 0,
        };
        if chosen.len() <= 1 && dead == 0 && !stale {
            return Ok(report);
        }
        let drop: Vec<String> = chosen
            .iter()
            .map(|&i| snap.segments[i].name.clone())
            .collect();
        let mut staged = StagedWrite::new(self, &mut w);
        for &si in &chosen {
            let r = &snap.segments[si].reader;
            for di in live(si) {
                let d = &r.docs[di as usize];
                let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
                let mut doc = SourceDoc::new(DocKey::new(r.str(d.key))?, r.doc_text(di)?);
                doc.kind = d.kind;
                doc.lang = Some(d.lang);
                doc.path = opt(r.str(d.path));
                doc.title = opt(r.str(d.title));
                doc.trust = d.trust;
                doc.scope = Scope::new(r.str(d.scope))?;
                doc.timestamp = Some(d.timestamp);
                doc.seq = d.seq;
                doc.group = opt(r.str(d.group));
                doc.role = d.role;
                doc.source_hash = Some(ContentDigest::from_raw(d.source_hash));
                staged.push(doc)?;
            }
        }
        let ingest = staged.commit(Vec::new(), None, &drop)?;
        if !ingest.skipped.is_empty() {
            log::warn!(
                "compaction dropped {} documents rejected by current compile options",
                ingest.skipped.len()
            );
        }
        report.generation = ingest.generation;
        report.documents = ingest.upserted;
        report.segments_after = durable.len() - chosen.len() + ingest.segments;
        report.dead_dropped = dead;
        Ok(report)
    }
}
