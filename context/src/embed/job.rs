//! The embedding job: give every live passage a vector, reusing vectors of
//! identical text, and carry vectors across compaction.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use super::table::{sidecar_key, sidecar_name, sidecar_segment, VectorTable};
use super::{
    check_vectors, embedder_key, passage_text, quantize, text_hash, Embedder, MAX_DIMS,
    MAX_ID_BYTES,
};
use crate::error::{ContextError, Result};
use crate::store::{files, ContextStore, StoreSnapshot};
use crate::types::PassageKind;

/// Settings for [`ContextStore::embed`].
#[derive(Debug, Clone)]
pub struct EmbedOptions {
    /// Texts per call to the embedder. Default: 32.
    pub batch: usize,
    /// Stop after embedding this many passages; `None` embeds everything.
    /// The next call continues where this one stopped.
    pub max_passages: Option<usize>,
    /// Delete vectors stored for other embedder ids. Default: true.
    pub prune_other_models: bool,
}

impl Default for EmbedOptions {
    fn default() -> Self {
        EmbedOptions {
            batch: 32,
            max_passages: None,
            prune_other_models: true,
        }
    }
}

/// Progress of an embedding run, reported after each batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct EmbedProgress {
    /// Passages embedded so far in this run.
    pub done: usize,
    /// Passages this run will embed.
    pub total: usize,
}

/// What an embedding run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EmbedReport {
    /// The embedder's id.
    pub model: String,
    /// Passages the embedder was called for.
    pub embedded: usize,
    /// Passages that got the vector of identical text embedded earlier.
    pub reused: usize,
    /// Live passages with a vector after the run.
    pub covered: usize,
    /// Live passages still without one (a budget, a cancel, or an error
    /// stopped the run); the next run continues with them.
    pub remaining: usize,
    /// The progress callback stopped the run. Work done before is kept.
    pub cancelled: bool,
}

struct Work {
    seg: usize,
    pid: u32,
    rank: u8,
    text: String,
    hash: u64,
}

/// Embed prose first: it is where questions and answers word things
/// differently. Code is better served by exact and lexical matching.
fn rank(kind: PassageKind) -> u8 {
    match kind {
        PassageKind::Prose | PassageKind::Turn => 0,
        PassageKind::Data => 1,
        PassageKind::Definition => 2,
        PassageKind::Code => 3,
    }
}

fn validate(embedder: &dyn Embedder) -> Result<()> {
    let id = embedder.id();
    if id.is_empty() || id.len() > MAX_ID_BYTES || id.chars().any(char::is_control) {
        return Err(ContextError::invalid(format!("bad embedder id {id:?}")));
    }
    let dims = embedder.dims();
    if dims == 0 || dims > MAX_DIMS {
        return Err(ContextError::invalid(format!("bad embedder dims {dims}")));
    }
    Ok(())
}

impl ContextStore {
    /// Give every live passage a vector from `embedder`, prose first. Text
    /// already embedded by this model (in any segment, under any key) is
    /// copied, not embedded again. Vectors are written next to their
    /// segments and used by every query from then on; a run stopped by
    /// [`EmbedOptions::max_passages`] or an error keeps what it embedded,
    /// and the next run continues.
    pub fn embed(&self, embedder: &dyn Embedder, opts: &EmbedOptions) -> Result<EmbedReport> {
        self.embed_with(embedder, opts, |_| true)
    }

    /// [`embed`](Self::embed), calling `progress` after each batch.
    /// Returning `false` stops the run early (`cancelled` in the report),
    /// keeping the vectors embedded so far.
    pub fn embed_with(
        &self,
        embedder: &dyn Embedder,
        opts: &EmbedOptions,
        mut progress: impl FnMut(&EmbedProgress) -> bool,
    ) -> Result<EmbedReport> {
        if self.is_read_only() {
            return Err(ContextError::ReadOnly);
        }
        validate(embedder)?;
        let (id, dims) = (embedder.id(), embedder.dims());
        let snap = self.snapshot();
        let existing = self.vectors.tables(self.dir(), &snap, id);
        let mut reuse: HashMap<u64, (Arc<VectorTable>, usize)> = HashMap::new();
        for t in existing.values() {
            for row in 0..t.rows() {
                if let Some((h, _, _)) = t.get(row) {
                    reuse.entry(h).or_insert((Arc::clone(t), row));
                }
            }
        }
        let mut report = EmbedReport {
            model: id.to_string(),
            ..EmbedReport::default()
        };
        let mut tables: HashMap<usize, VectorTable> = HashMap::new();
        let mut work: Vec<Work> = Vec::new();
        for (si, seg) in snap.segments.iter().enumerate().filter(|(_, s)| s.durable) {
            let r = &seg.reader;
            let current = existing.get(&seg.name);
            for (di, d) in r.docs.iter().enumerate() {
                if !snap.doc_live(si as u32, di as u32) {
                    continue;
                }
                for pid in d.first_passage..d.first_passage + d.passage_count {
                    if current.is_some_and(|t| t.get(pid as usize).is_some()) {
                        report.covered += 1;
                        continue;
                    }
                    let body = r.passage_text(pid)?;
                    if body.trim().is_empty() {
                        continue;
                    }
                    let p = &r.passages[pid as usize];
                    let symbol = Some(r.str(p.symbol)).filter(|s| !s.is_empty());
                    let text = passage_text(r.str(p.breadcrumb), symbol, &body);
                    let hash = text_hash(&text);
                    let table = tables.entry(si).or_insert_with(|| {
                        current.map_or_else(
                            || VectorTable::empty(id, dims, r.passages.len()),
                            |t| (**t).clone(),
                        )
                    });
                    if let Some((src, row)) = reuse.get(&hash) {
                        let (_, scale, q) = src.get(*row).expect("reuse rows are embedded");
                        table.set(pid as usize, hash, scale, q);
                        report.reused += 1;
                        report.covered += 1;
                        continue;
                    }
                    work.push(Work {
                        seg: si,
                        pid,
                        rank: rank(p.kind),
                        text,
                        hash,
                    });
                }
            }
        }
        work.sort_by_key(|w| (w.rank, w.seg, w.pid));
        // Identical text queued twice is embedded once.
        let mut unique: Vec<usize> = Vec::new();
        let mut first_of: HashMap<u64, usize> = HashMap::new();
        for (i, w) in work.iter().enumerate() {
            first_of.entry(w.hash).or_insert_with(|| {
                unique.push(i);
                i
            });
        }
        unique.truncate(opts.max_passages.unwrap_or(usize::MAX));
        let total = unique.len();
        let mut vectors: HashMap<u64, (f32, Vec<i8>)> = HashMap::new();
        let mut outcome = Ok(());
        let mut done = 0;
        for chunk in unique.chunks(opts.batch.max(1)) {
            let texts: Vec<&str> = chunk.iter().map(|&i| work[i].text.as_str()).collect();
            let got = embedder
                .embed_documents(&texts)
                .and_then(|v| check_vectors(&v, texts.len(), dims).map(|()| v));
            match got {
                Ok(vs) => {
                    for (&i, v) in chunk.iter().zip(vs) {
                        if let Some(q) = quantize(&v) {
                            vectors.insert(work[i].hash, q);
                        }
                    }
                }
                Err(e) => {
                    outcome = Err(ContextError::Embedder(e));
                    break;
                }
            }
            done += chunk.len();
            report.embedded = done;
            if !progress(&EmbedProgress { done, total }) {
                report.cancelled = true;
                break;
            }
        }
        for w in &work {
            match vectors.get(&w.hash) {
                Some((scale, q)) => {
                    let table = tables.get_mut(&w.seg).expect("table staged with its work");
                    table.set(w.pid as usize, w.hash, *scale, q);
                    report.covered += 1;
                }
                None => report.remaining += 1,
            }
        }
        let mut written = Vec::new();
        for (si, table) in tables {
            let name = &snap.segments[si].name;
            let file = sidecar_name(name, &embedder_key(id));
            files::write_atomic(self.dir(), &file, &table.encode(), self.sync_writes())?;
            written.push((name.clone(), Arc::new(table)));
        }
        self.vectors.insert(id, written);
        if opts.prune_other_models {
            prune_other_models(self.dir(), &embedder_key(id));
        }
        outcome.map(|()| report)
    }
}

/// Remove sidecars of other embedder keys. Best effort.
fn prune_other_models(dir: &std::path::Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if sidecar_key(&name).is_some_and(|k| k != keep) {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                log::debug!("could not remove {name}: {e}");
            }
        }
    }
}

/// One embedder's vectors, by text hash.
struct CarriedModel {
    key: String,
    id: String,
    dims: usize,
    vectors: HashMap<u64, (f32, Vec<i8>)>,
}

/// Vectors of segments about to be dropped, per embedder: kept in memory
/// across a compaction so the new segments get them without a model.
pub(crate) struct Carried {
    models: Vec<CarriedModel>,
}

/// Read the vectors of `segments` (file names) before they are dropped.
pub(crate) fn collect(dir: &std::path::Path, snap: &StoreSnapshot, segments: &[String]) -> Carried {
    let mut by_key: HashMap<String, CarriedModel> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Carried { models: Vec::new() };
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let (Some(seg), Some(key)) = (sidecar_segment(&name), sidecar_key(&name)) else {
            continue;
        };
        if !segments.contains(&seg) {
            continue;
        }
        let Some(handle) = snap.segments.iter().find(|s| s.name == seg) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Some(id) = super::table::sidecar_id(&bytes) else {
            continue;
        };
        let Ok(t) = VectorTable::decode(&bytes, &id, handle.reader.passages.len()) else {
            continue;
        };
        let model = by_key
            .entry(key.to_string())
            .or_insert_with(|| CarriedModel {
                key: key.to_string(),
                id: id.clone(),
                dims: t.dims,
                vectors: HashMap::new(),
            });
        for row in 0..t.rows() {
            if let Some((h, scale, q)) = t.get(row) {
                model
                    .vectors
                    .entry(h)
                    .or_insert_with(|| (scale, q.to_vec()));
            }
        }
    }
    Carried {
        models: by_key.into_values().collect(),
    }
}

impl ContextStore {
    /// Write sidecars for the segments of `snap` not in `before`, from
    /// vectors `carried` over a compaction. Best effort: a failure costs
    /// re-embedding, never correctness.
    pub(crate) fn carry_vectors(&self, carried: &Carried, before: &[String], snap: &StoreSnapshot) {
        for CarriedModel {
            key,
            id,
            dims,
            vectors,
        } in &carried.models
        {
            let mut written = Vec::new();
            for seg in snap
                .segments
                .iter()
                .filter(|s| s.durable && !before.contains(&s.name))
            {
                let r = &seg.reader;
                let mut table = VectorTable::empty(id, *dims, r.passages.len());
                for pid in 0..r.passages.len() {
                    let Ok(body) = r.passage_text(pid as u32) else {
                        continue;
                    };
                    let p = &r.passages[pid];
                    let symbol = Some(r.str(p.symbol)).filter(|s| !s.is_empty());
                    let hash = text_hash(&passage_text(r.str(p.breadcrumb), symbol, &body));
                    if let Some((scale, q)) = vectors.get(&hash) {
                        table.set(pid, hash, *scale, q);
                    }
                }
                if table.present() == 0 {
                    continue;
                }
                let file = sidecar_name(&seg.name, key);
                match files::write_atomic(self.dir(), &file, &table.encode(), self.sync_writes()) {
                    Ok(()) => written.push((seg.name.clone(), Arc::new(table))),
                    Err(e) => log::warn!("could not carry vectors to {file}: {e}"),
                }
            }
            self.vectors.insert(id, written);
        }
    }
}
