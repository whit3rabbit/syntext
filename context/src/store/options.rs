//! Store options and reports.

use serde::Serialize;

use crate::compile::{CompileOptions, SkipReason};
use crate::types::DocKey;

/// Runtime options for opening or creating a store.
#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// Compile settings for a *new* store. An existing store keeps the
    /// options persisted in its manifest so all its segments agree.
    pub compile: CompileOptions,
    /// Fsync segments, manifests, and journal appends. Default: true.
    pub sync: bool,
    /// Source text compiled into one segment at a time. Bounds the memory
    /// of large writes (peak use is a small multiple of this) and sets the
    /// size of a full segment. Default: 32 MiB.
    pub batch_bytes: usize,
    /// Merge small segments (under about half a batch) once there are more
    /// than this many. Default: 8.
    pub max_segments: usize,
    /// Compact when this fraction of documents is dead. Default: 0.3.
    pub max_dead_ratio: f32,
    /// Conversation turns kept in the live journal before they are
    /// compacted into a durable segment. Default: 32.
    pub memory_flush_turns: usize,
    /// Checksum the large TEXT and POSTINGS sections of every segment at
    /// open. Default: false (metadata sections are always checked).
    pub verify_on_open: bool,
}

impl Default for StoreOptions {
    fn default() -> Self {
        StoreOptions {
            compile: CompileOptions::default(),
            sync: true,
            batch_bytes: 32 * 1024 * 1024,
            max_segments: 8,
            max_dead_ratio: 0.3,
            memory_flush_turns: 32,
            verify_on_open: false,
        }
    }
}

/// Outcome of an [`ContextStore::apply`] (or `upsert` / `remove`).
#[derive(Debug, Clone, Serialize)]
pub struct IngestReport {
    /// Store generation after the write (unchanged when nothing changed).
    pub generation: u64,
    /// Documents compiled and published.
    pub upserted: usize,
    /// Existing documents removed (including superseded skips).
    pub removed: usize,
    /// Passages in the published documents.
    pub passages: usize,
    /// Segments written.
    pub segments: usize,
    /// Documents that could not be compiled, and why.
    pub skipped: Vec<(DocKey, SkipReason)>,
}

/// Point-in-time store statistics.
#[derive(Debug, Clone, Serialize)]
pub struct StoreStats {
    /// Current generation.
    pub generation: u64,
    /// Live documents.
    pub documents: usize,
    /// Live passages.
    pub passages: u64,
    /// Durable segments.
    pub segments: usize,
    /// Conversation turns held only in the live journal.
    pub live_turns: usize,
    /// Bytes across durable segment files.
    pub bytes: u64,
    /// Superseded or removed documents awaiting compaction.
    pub dead_documents: u64,
    /// Graph entities across segments.
    pub entities: u64,
    /// Durable segments compiled by an older compiler version; the next
    /// compaction recompiles them.
    pub stale_segments: usize,
}
