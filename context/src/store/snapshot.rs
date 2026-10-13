//! [`StoreSnapshot`]: an immutable, consistent view of a store.
//!
//! Readers load the current snapshot with one atomic pointer read and keep
//! it for the whole query, so a concurrent write never changes the corpus
//! under a retrieval. Segments are shared between snapshots by `Arc`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use super::reader::{PassageRec, SegmentReader};
use crate::error::Result;
use crate::structure::Lang;
use crate::types::{DocKind, Role, Trust};

/// One segment as loaded into a snapshot.
pub(crate) struct SegmentHandle {
    /// File name, or `"live"` for the in-memory memory segment.
    pub name: String,
    pub durable: bool,
    pub reader: SegmentReader,
}

/// Bitset of dead local document ids.
#[derive(Clone, Debug, Default)]
pub(crate) struct DeadSet {
    bits: Vec<u64>,
}

impl DeadSet {
    fn new(n: usize) -> Self {
        DeadSet {
            bits: vec![0; n.div_ceil(64)],
        }
    }

    fn set(&mut self, i: u32) {
        if let Some(w) = self.bits.get_mut(i as usize / 64) {
            *w |= 1 << (i % 64);
        }
    }

    pub(crate) fn contains(&self, i: u32) -> bool {
        self.bits
            .get(i as usize / 64)
            .is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    fn count(&self) -> u64 {
        self.bits.iter().map(|w| u64::from(w.count_ones())).sum()
    }
}

/// A document's location in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DocLoc {
    pub seg: u32,
    pub doc: u32,
}

/// A passage's location in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PassageLoc {
    pub seg: u32,
    pub pid: u32,
}

/// Summary of one live document.
#[derive(Debug, Clone, Serialize)]
pub struct DocSummary {
    /// Store-unique key.
    pub key: String,
    /// Title.
    pub title: String,
    /// Repository-relative path, for files.
    pub path: Option<String>,
    /// Visibility scope.
    pub scope: String,
    /// Grouping label (session, source label).
    pub group: Option<String>,
    /// Origin kind.
    pub kind: DocKind,
    /// Trust level.
    pub trust: Trust,
    /// Detected language.
    pub lang: Lang,
    /// Speaker, for turns.
    pub role: Option<Role>,
    /// Unix seconds, 0 when unknown.
    pub timestamp: i64,
    /// Ordering sequence (journal sequence for turns).
    pub seq: u64,
    /// Number of passages.
    pub passages: u32,
    /// Hex BLAKE3 digest of the original source bytes.
    pub source_hash: String,
    /// Hex BLAKE3 digest of the stored text.
    pub text_hash: String,
}

/// An immutable view of a store at one generation.
pub struct StoreSnapshot {
    pub(crate) generation: u64,
    pub(crate) store_id: String,
    pub(crate) segments: Vec<Arc<SegmentHandle>>,
    pub(crate) dead: Vec<DeadSet>,
    pub(crate) doc_index: HashMap<String, DocLoc>,
    pub(crate) live_passages: u64,
    pub(crate) total_length: u64,
    /// Highest conversation-turn sequence held by the live segment.
    pub(crate) live_through: u64,
}

impl std::fmt::Debug for StoreSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreSnapshot")
            .field("generation", &self.generation)
            .field("segments", &self.segments.len())
            .field("documents", &self.doc_index.len())
            .finish()
    }
}

impl StoreSnapshot {
    /// Assemble a snapshot. `manifest_dead[i]` lists dead local docs of
    /// `segments[i]` (durable segments only). A key present in several
    /// segments resolves to the last one; earlier copies are treated as dead.
    pub(crate) fn build(
        generation: u64,
        store_id: String,
        segments: Vec<Arc<SegmentHandle>>,
        manifest_dead: &[&[u32]],
    ) -> Self {
        let mut dead: Vec<DeadSet> = segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut d = DeadSet::new(s.reader.docs.len());
                for &x in manifest_dead.get(i).copied().unwrap_or(&[]) {
                    d.set(x);
                }
                d
            })
            .collect();
        let mut doc_index: HashMap<String, DocLoc> = HashMap::new();
        for (si, seg) in segments.iter().enumerate() {
            for (di, doc) in seg.reader.docs.iter().enumerate() {
                let loc = DocLoc {
                    seg: si as u32,
                    doc: di as u32,
                };
                if dead[si].contains(loc.doc) {
                    continue;
                }
                let key = seg.reader.str(doc.key).to_string();
                if let Some(prev) = doc_index.insert(key, loc) {
                    dead[prev.seg as usize].set(prev.doc);
                }
            }
        }
        let mut live_passages = 0u64;
        let mut total_length = 0u64;
        for (si, seg) in segments.iter().enumerate() {
            for (di, doc) in seg.reader.docs.iter().enumerate() {
                if dead[si].contains(di as u32) {
                    continue;
                }
                let range =
                    doc.first_passage as usize..(doc.first_passage + doc.passage_count) as usize;
                live_passages += u64::from(doc.passage_count);
                total_length += seg.reader.passages[range]
                    .iter()
                    .map(|p| u64::from(p.length))
                    .sum::<u64>();
            }
        }
        StoreSnapshot {
            generation,
            store_id,
            segments,
            dead,
            doc_index,
            live_passages,
            total_length,
            live_through: 0,
        }
    }

    /// Store generation this snapshot reflects.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Store identifier (stable for the store's lifetime).
    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    /// Number of live documents.
    pub fn document_count(&self) -> usize {
        self.doc_index.len()
    }

    /// Number of live passages.
    pub fn passage_count(&self) -> u64 {
        self.live_passages
    }

    /// Summary of the live document with `key`.
    pub fn document(&self, key: &str) -> Option<DocSummary> {
        self.doc_index.get(key).map(|&loc| self.summary(loc))
    }

    /// Summaries of all live documents, sorted by key.
    pub fn documents(&self) -> Vec<DocSummary> {
        let mut locs: Vec<(&String, DocLoc)> =
            self.doc_index.iter().map(|(k, v)| (k, *v)).collect();
        locs.sort_by(|a, b| a.0.cmp(b.0));
        locs.into_iter().map(|(_, loc)| self.summary(loc)).collect()
    }

    /// Full stored text of the live document with `key`.
    pub fn document_text(&self, key: &str) -> Result<Option<String>> {
        match self.doc_index.get(key) {
            Some(&loc) => self.reader(loc.seg).doc_text(loc.doc).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn summary(&self, loc: DocLoc) -> DocSummary {
        let r = self.reader(loc.seg);
        let d = &r.docs[loc.doc as usize];
        let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
        DocSummary {
            key: r.str(d.key).to_string(),
            title: r.str(d.title).to_string(),
            path: opt(r.str(d.path)),
            scope: r.str(d.scope).to_string(),
            group: opt(r.str(d.group)),
            kind: d.kind,
            trust: d.trust,
            lang: d.lang,
            role: d.role,
            timestamp: d.timestamp,
            seq: d.seq,
            passages: d.passage_count,
            source_hash: hex(&d.source_hash),
            text_hash: hex(&d.text_hash),
        }
    }

    pub(crate) fn reader(&self, seg: u32) -> &SegmentReader {
        &self.segments[seg as usize].reader
    }

    pub(crate) fn lookup(&self, key: &str) -> Option<DocLoc> {
        self.doc_index.get(key).copied()
    }

    pub(crate) fn doc_live(&self, seg: u32, doc: u32) -> bool {
        !self.dead[seg as usize].contains(doc)
    }

    pub(crate) fn passage(&self, loc: PassageLoc) -> &PassageRec {
        &self.reader(loc.seg).passages[loc.pid as usize]
    }

    /// Average passage length (BM25 `avgdl`), at least 1.
    pub(crate) fn avg_len(&self) -> f32 {
        if self.live_passages == 0 {
            1.0
        } else {
            (self.total_length as f32 / self.live_passages as f32).max(1.0)
        }
    }

    /// Document frequency of `term` summed over segments. Dead documents
    /// still count until compaction; IDF is only slightly understated.
    pub(crate) fn df(&self, term: &str) -> u64 {
        self.segments
            .iter()
            .filter_map(|s| {
                s.reader
                    .term_index(term)
                    .map(|i| u64::from(s.reader.term_df(i)))
            })
            .sum()
    }

    /// Number of dead documents across segments.
    pub(crate) fn dead_count(&self) -> u64 {
        self.dead.iter().map(DeadSet::count).sum()
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xF) as usize] as char);
    }
    s
}
