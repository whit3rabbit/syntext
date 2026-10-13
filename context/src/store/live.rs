//! Conversation memory inside the store: journal appends, the live
//! in-memory segment, and compaction of turns into durable segments.

use std::sync::{Arc, TryLockError};

use super::journal::Turn;
use super::manifest::unix_now;
use super::reader::SegmentReader;
use super::snapshot::SegmentHandle;
use super::write::StagedWrite;
use super::writer::write_segment;
use super::{lock, ContextStore, Memory, Writable, Writer};
use crate::compile::{compile_all, CompileOptions, SourceDoc};
use crate::error::{ContextError, Result};
use crate::structure::Lang;
use crate::types::{DocKey, DocKind, Role, Scope, Trust};

/// Longest accepted session identifier.
const MAX_SESSION_BYTES: usize = 128;

/// Document key of a turn: `turn:<seq>` (sequences are store-unique).
pub(crate) fn turn_key(seq: u64) -> DocKey {
    // A formatted u64 is always a valid key.
    DocKey::new(format!("turn:{seq}")).expect("turn keys are valid")
}

/// The document a turn is indexed as.
pub(crate) fn turn_doc(t: &Turn) -> SourceDoc {
    let mut doc = SourceDoc::new(turn_key(t.seq), t.text.clone());
    doc.kind = DocKind::Turn;
    doc.trust = Trust::Memory;
    doc.scope = t.scope.clone();
    doc.timestamp = Some(t.timestamp);
    doc.seq = t.seq;
    doc.group = Some(t.session.clone());
    doc.role = Some(t.role);
    doc.lang = Some(Lang::Markdown);
    doc.title = Some(format!(
        "{} turn {} (session {})",
        t.role.as_str(),
        t.seq,
        t.session
    ));
    doc
}

/// Compile `turns` into an in-memory segment, or `None` when empty.
pub(crate) fn build_live(
    turns: &[Turn],
    compile: &CompileOptions,
) -> Result<Option<Arc<SegmentHandle>>> {
    if turns.is_empty() {
        return Ok(None);
    }
    let (docs, _skipped) = compile_all(turns.iter().map(turn_doc).collect(), compile);
    let (bytes, _) = write_segment(&docs, compile.graph, unix_now())?;
    Ok(Some(Arc::new(SegmentHandle {
        name: "live".to_string(),
        durable: false,
        reader: SegmentReader::from_bytes(Arc::from(bytes))?,
    })))
}

impl ContextStore {
    /// Durably record one conversation turn and make it searchable.
    ///
    /// The turn is synced to the journal before this returns, then indexed
    /// in the live segment. About every `memory_flush_turns` turns the live
    /// set is compacted into a durable segment. Recording never waits on a
    /// directory sync or compaction: when one holds the segment lock, the
    /// flush is left for a later turn (the turns are already durable in the
    /// journal). `timestamp` defaults to now.
    pub fn append_turn(
        &self,
        session: &str,
        role: Role,
        scope: Scope,
        text: &str,
        timestamp: Option<i64>,
    ) -> Result<Turn> {
        if session.is_empty()
            || session.len() > MAX_SESSION_BYTES
            || session.chars().any(char::is_control)
        {
            return Err(ContextError::invalid(
                "session id must be 1..=128 bytes without control characters",
            ));
        }
        let w = self.writable()?;
        let mut m = lock(&w.memory);
        let turn = Turn {
            seq: m.next_seq,
            session: session.to_string(),
            role,
            scope,
            text: text.to_string(),
            timestamp: timestamp.unwrap_or_else(unix_now),
        };
        m.journal.append(&turn)?;
        m.next_seq += 1;
        m.live.push(turn.clone());
        let mut flushed = false;
        if m.live.len() >= self.options.memory_flush_turns.max(1) {
            let seg = match w.segments.try_lock() {
                Ok(seg) => Some(seg),
                Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some(mut seg) = seg {
                flushed = self.flush_memory_locked(w, &mut m, &mut seg)?;
            }
        }
        if !flushed {
            let handle = build_live(&m.live, &self.compile)?;
            self.publish(w, |p| p.live = handle);
        }
        Ok(turn)
    }

    /// Compact all live turns into a durable segment now (waiting for any
    /// running sync or compaction). Returns whether anything was flushed.
    pub fn flush_memory(&self) -> Result<bool> {
        let w = self.writable()?;
        let mut m = lock(&w.memory);
        let mut seg = lock(&w.segments);
        self.flush_memory_locked(w, &mut m, &mut seg)
    }

    /// Turns currently held only in the live journal (not yet compacted).
    pub fn live_turns(&self) -> Vec<Turn> {
        match &self.writable {
            Some(w) => lock(&w.memory).live.clone(),
            None => Vec::new(),
        }
    }

    fn flush_memory_locked(&self, w: &Writable, m: &mut Memory, seg: &mut Writer) -> Result<bool> {
        let Some(through) = m.live.last().map(|t| t.seq) else {
            return Ok(false);
        };
        let mut staged = StagedWrite::new(self, w, seg);
        for doc in m.live.iter().map(turn_doc) {
            staged.push(doc)?;
        }
        staged.commit(Vec::new(), Some(through), &[])?;
        m.live.clear();
        // The manifest now records `through`; replay skips older records,
        // so a failed rotation only costs journal space.
        if let Err(e) = m.journal.rewrite(&self.dir, &[]) {
            log::warn!("memory journal rotation failed: {e}");
        }
        Ok(true)
    }
}
