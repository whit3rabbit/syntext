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
mod publish;
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
use publish::{open_durable, Published};
use snapshot::SegmentHandle;

/// Segment-writing state: the manifest and the inter-process writer lock.
struct Writer {
    _lock: File,
    manifest: Manifest,
}

/// Conversation-memory state. Locked separately from segment writes, so
/// recording a turn never waits on a directory sync or a compaction.
struct Memory {
    journal: Journal,
    live: Vec<Turn>,
    next_seq: u64,
}

/// What a writable handle adds. Locks are always taken in field order:
/// `memory`, then `segments`, then `published`.
struct Writable {
    memory: Mutex<Memory>,
    segments: Mutex<Writer>,
    published: Mutex<Published>,
}

/// A context store handle. `Send + Sync`; share it behind an `Arc`.
pub struct ContextStore {
    dir: PathBuf,
    options: StoreOptions,
    compile: CompileOptions,
    snapshot: ArcSwap<StoreSnapshot>,
    writable: Option<Writable>,
}

/// Lock `m`, recovering the data of a poisoned lock: every critical section
/// leaves the state consistent before anything can panic.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl std::fmt::Debug for ContextStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextStore")
            .field("dir", &self.dir)
            .field("read_only", &self.writable.is_none())
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
        if let Some(w) = &store.writable {
            files::gc(&store.dir, &self::lock(&w.segments).manifest);
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
        let (snapshot, compile) = Self::load_read_only(&dir, &options, None)?;
        Ok(ContextStore {
            dir,
            options,
            compile,
            snapshot: ArcSwap::from_pointee(snapshot),
            writable: None,
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
        let compile = manifest.compile;
        let live_handle = live::build_live(&live, &compile)?;
        let durable = open_durable(&dir, &options, &manifest, None)?;
        let published = Published::new(&manifest, durable, live_handle);
        let snapshot = published.snapshot();
        let next_seq = live.last().map_or(manifest.memory_through, |t| {
            t.seq.max(manifest.memory_through)
        }) + 1;
        Ok(ContextStore {
            dir,
            options,
            compile,
            snapshot: ArcSwap::from_pointee(snapshot),
            writable: Some(Writable {
                memory: Mutex::new(Memory {
                    journal,
                    live,
                    next_seq,
                }),
                segments: Mutex::new(Writer {
                    _lock: lock,
                    manifest,
                }),
                published: Mutex::new(published),
            }),
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
    ) -> Result<(StoreSnapshot, CompileOptions)> {
        let mut last_err = None;
        for _ in 0..5 {
            let manifest =
                Manifest::load(dir)?.ok_or_else(|| ContextError::NotFound(dir.to_path_buf()))?;
            let turns: Vec<Turn> = Journal::read(dir)?
                .into_iter()
                .filter(|t| t.seq > manifest.memory_through)
                .collect();
            let live = live::build_live(&turns, &manifest.compile)?;
            match open_durable(dir, options, &manifest, prev) {
                Ok(durable) => {
                    let snap = Published::new(&manifest, durable, live).snapshot();
                    return Ok((snap, manifest.compile));
                }
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
        self.writable.is_none()
    }

    /// The current snapshot. Cheap (one atomic load); hold it for the
    /// duration of a query.
    pub fn snapshot(&self) -> Arc<StoreSnapshot> {
        self.snapshot.load_full()
    }

    /// Compile options in effect for this store (fixed at creation).
    pub fn compile_options(&self) -> CompileOptions {
        self.compile
    }

    fn writable(&self) -> Result<&Writable> {
        self.writable.as_ref().ok_or(ContextError::ReadOnly)
    }

    /// Re-read the manifest and journal written by another process. Returns
    /// whether the snapshot changed. A no-op for writable handles, which
    /// are the only writer.
    pub fn reload(&self) -> Result<bool> {
        if self.writable.is_some() {
            return Ok(false);
        }
        let cur = self.snapshot();
        let (next, _) = Self::load_read_only(&self.dir, &self.options, Some(&cur))?;
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
