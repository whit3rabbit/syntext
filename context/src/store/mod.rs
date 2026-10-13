//! The context store: durable, generation-versioned segments plus live
//! conversation memory.
//!
//! A store is a directory:
//!
//! ```text
//! <store>/
//!   manifest.json        live segment list, generation, compile options
//!   seg-*.ctxs           immutable segments (own binary format, see `format`)
//!   journal.log          conversation turns not yet compacted into a segment
//!   sources/<label>/     per-directory-source change catalogue (syntext)
//!   write.lock           exclusive writer lock
//! ```
//!
//! Writes compile documents into a new segment, mark superseded documents
//! dead, and publish a new manifest atomically. Readers hold an immutable
//! [`StoreSnapshot`] for the duration of a query.

mod backing;
mod compact;
mod files;
mod format;
mod journal;
mod live;
pub(crate) mod manifest;
mod options;
pub(crate) mod reader;
mod reader_graph;
mod reader_records;
mod snapshot;
mod write;
mod writer;
mod writer_graph;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use arc_swap::ArcSwap;

pub use compact::CompactReport;
pub use journal::Turn;
pub(crate) use live::turn_key;
pub use options::{IngestReport, StoreOptions, StoreStats};
pub(crate) use snapshot::{DocLoc, PassageLoc};
pub use snapshot::{DocSummary, StoreSnapshot};

use crate::compile::CompileOptions;
use crate::error::{ContextError, Result};
use journal::Journal;
use manifest::Manifest;
use reader::SegmentReader;
use snapshot::SegmentHandle;

/// Mutable state of a writable handle.
struct Writer {
    _lock: File,
    manifest: Manifest,
    journal: Journal,
    live: Vec<Turn>,
    live_handle: Option<Arc<SegmentHandle>>,
}

/// A context store handle. `Send + Sync`; share it behind an `Arc`.
pub struct ContextStore {
    dir: PathBuf,
    options: StoreOptions,
    snapshot: ArcSwap<StoreSnapshot>,
    writer: Option<Mutex<Writer>>,
}

impl std::fmt::Debug for ContextStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextStore")
            .field("dir", &self.dir)
            .field("read_only", &self.writer.is_none())
            .finish()
    }
}

impl ContextStore {
    /// Create a new, empty store in `dir` (created if missing). Fails if a
    /// store already exists there.
    pub fn create(dir: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let lock = files::acquire_lock(&dir)?;
        if Manifest::load(&dir)?.is_some() {
            return Err(ContextError::invalid(format!(
                "a context store already exists at {}",
                dir.display()
            )));
        }
        let mut manifest = Manifest::new(options.compile);
        manifest.save(&dir, options.sync)?;
        let journal = Journal::create(&dir, &[], options.sync)?;
        Self::finish_open(dir, options, manifest, lock, journal, Vec::new())
    }

    /// Open an existing store for reading and writing (takes the writer
    /// lock for the handle's lifetime).
    pub fn open(dir: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.is_dir() {
            return Err(ContextError::NotFound(dir));
        }
        let lock = files::acquire_lock(&dir)?;
        let manifest = Manifest::load(&dir)?.ok_or_else(|| ContextError::NotFound(dir.clone()))?;
        let (journal, turns) = Journal::open(&dir, options.sync)?;
        let live: Vec<Turn> = turns
            .into_iter()
            .filter(|t| t.seq > manifest.memory_through)
            .collect();
        let store = Self::finish_open(dir, options, manifest, lock, journal, live)?;
        if let Some(w) = &store.writer {
            let w = w.lock().unwrap_or_else(|e| e.into_inner());
            files::gc(&store.dir, &w.manifest);
        }
        Ok(store)
    }

    /// Open `dir`, creating a store with `options` if none exists.
    pub fn open_or_create(dir: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let dir = dir.as_ref();
        if dir.join(manifest::MANIFEST_FILE).is_file() {
            Self::open(dir, options)
        } else {
            Self::create(dir, options)
        }
    }

    /// Open without the writer lock. Writes fail with
    /// [`ContextError::ReadOnly`]; call [`reload`](Self::reload) to pick up
    /// generations published by a writer in another process.
    pub fn open_read_only(dir: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let snapshot = Self::load_read_only(&dir, &options, None)?;
        Ok(ContextStore {
            dir,
            options,
            snapshot: ArcSwap::from_pointee(snapshot),
            writer: None,
        })
    }

    fn finish_open(
        dir: PathBuf,
        options: StoreOptions,
        manifest: Manifest,
        lock: File,
        journal: Journal,
        live: Vec<Turn>,
    ) -> Result<Self> {
        let live_handle = live::build_live(&live, &manifest.compile)?;
        let snapshot = assemble(&dir, &options, &manifest, None, live_handle.clone())?;
        Ok(ContextStore {
            dir,
            options,
            snapshot: ArcSwap::from_pointee(snapshot),
            writer: Some(Mutex::new(Writer {
                _lock: lock,
                manifest,
                journal,
                live,
                live_handle,
            })),
        })
    }

    /// Load manifest, segments, and journal without locking. A writer in
    /// another process may delete superseded segments between reading the
    /// manifest and opening them, so a missing segment retries from a fresh
    /// manifest.
    fn load_read_only(
        dir: &Path,
        options: &StoreOptions,
        prev: Option<&StoreSnapshot>,
    ) -> Result<StoreSnapshot> {
        let mut last_err = None;
        for _ in 0..5 {
            let manifest =
                Manifest::load(dir)?.ok_or_else(|| ContextError::NotFound(dir.to_path_buf()))?;
            let turns: Vec<Turn> = Journal::read(dir)?
                .into_iter()
                .filter(|t| t.seq > manifest.memory_through)
                .collect();
            let live = live::build_live(&turns, &manifest.compile)?;
            match assemble(dir, options, &manifest, prev, live) {
                Ok(s) => return Ok(s),
                Err(ContextError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    last_err = Some(ContextError::Io(e));
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| ContextError::NotFound(dir.to_path_buf())))
    }

    /// Store directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// True for handles opened with [`open_read_only`](Self::open_read_only).
    pub fn is_read_only(&self) -> bool {
        self.writer.is_none()
    }

    /// The current snapshot. Cheap (one atomic load); hold it for the
    /// duration of a query.
    pub fn snapshot(&self) -> Arc<StoreSnapshot> {
        self.snapshot.load_full()
    }

    /// Compile options in effect for this store.
    pub fn compile_options(&self) -> CompileOptions {
        match &self.writer {
            Some(w) => w.lock().unwrap_or_else(|e| e.into_inner()).manifest.compile,
            None => Manifest::load(&self.dir)
                .ok()
                .flatten()
                .map_or(self.options.compile, |m| m.compile),
        }
    }

    fn writer(&self) -> Result<MutexGuard<'_, Writer>> {
        let w = self.writer.as_ref().ok_or(ContextError::ReadOnly)?;
        Ok(w.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Re-read the manifest and journal written by another process. Returns
    /// whether the snapshot changed. A no-op for writable handles, which
    /// are the only writer.
    pub fn reload(&self) -> Result<bool> {
        if self.writer.is_some() {
            return Ok(false);
        }
        let cur = self.snapshot();
        let next = Self::load_read_only(&self.dir, &self.options, Some(&cur))?;
        let changed = next.generation != cur.generation || next.live_through != cur.live_through;
        if changed {
            self.snapshot.store(Arc::new(next));
        }
        Ok(changed)
    }

    /// Current statistics.
    pub fn stats(&self) -> StoreStats {
        let snap = self.snapshot();
        let durable: Vec<&Arc<SegmentHandle>> =
            snap.segments.iter().filter(|s| s.durable).collect();
        StoreStats {
            generation: snap.generation,
            documents: snap.document_count(),
            passages: snap.passage_count(),
            segments: durable.len(),
            live_turns: snap
                .segments
                .iter()
                .filter(|s| !s.durable)
                .map(|s| s.reader.docs.len())
                .sum(),
            bytes: durable.iter().map(|s| s.reader.byte_len()).sum(),
            dead_documents: snap.dead_count(),
            entities: snap
                .segments
                .iter()
                .map(|s| u64::from(s.reader.meta.entities))
                .sum(),
            stale_segments: durable
                .iter()
                .filter(|s| s.reader.meta.compiler != crate::compile::COMPILER_VERSION)
                .count(),
        }
    }

    /// Fully verify the manifest checksum and every segment section
    /// checksum, including the large text and postings sections.
    pub fn verify(&self) -> Result<()> {
        Manifest::load(&self.dir)?.ok_or_else(|| ContextError::NotFound(self.dir.clone()))?;
        for seg in &self.snapshot().segments {
            seg.reader.verify_bulk()?;
        }
        Ok(())
    }
}

/// Build a snapshot for `manifest`, reusing already-open segments from
/// `prev` and appending the live memory segment.
fn assemble(
    dir: &Path,
    options: &StoreOptions,
    manifest: &Manifest,
    prev: Option<&StoreSnapshot>,
    live: Option<Arc<SegmentHandle>>,
) -> Result<StoreSnapshot> {
    let mut segments: Vec<Arc<SegmentHandle>> = Vec::with_capacity(manifest.segments.len() + 1);
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
    let dead: Vec<&[u32]> = manifest
        .segments
        .iter()
        .map(|e| e.dead.as_slice())
        .collect();
    let live_through = live.as_ref().map_or(0, |h| {
        h.reader.docs.iter().map(|d| d.seq).max().unwrap_or(0)
    });
    if let Some(h) = live {
        segments.push(h);
    }
    let mut snap = StoreSnapshot::build(
        manifest.generation,
        manifest.store_id.clone(),
        segments,
        &dead,
    );
    snap.live_through = live_through;
    Ok(snap)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
