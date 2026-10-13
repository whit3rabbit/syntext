//! Conversation memory inside the store: journal appends, the live
//! in-memory segment, and compaction of turns into durable segments.

use std::sync::Arc;

use super::journal::Turn;
use super::manifest::unix_now;
use super::reader::SegmentReader;
use super::snapshot::SegmentHandle;
use super::write::StagedWrite;
use super::writer::write_segment;
use super::{assemble, ContextStore, Writer};
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
    /// in the live segment. Every `memory_flush_turns` turns the live set is
    /// compacted into a durable segment. `timestamp` defaults to now.
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
        let mut w = self.writer()?;
        let seq = w.live.last().map_or(w.manifest.memory_through, |t| {
            t.seq.max(w.manifest.memory_through)
        }) + 1;
        let turn = Turn {
            seq,
            session: session.to_string(),
            role,
            scope,
            text: text.to_string(),
            timestamp: timestamp.unwrap_or_else(unix_now),
        };
        w.journal.append(&turn)?;
        w.live.push(turn.clone());
        if w.live.len() >= self.options.memory_flush_turns.max(1) {
            self.flush_memory_locked(&mut w)?;
        } else {
            self.refresh_live(&mut w)?;
        }
        Ok(turn)
    }

    /// Compact all live turns into a durable segment now. Returns whether
    /// anything was flushed.
    pub fn flush_memory(&self) -> Result<bool> {
        let mut w = self.writer()?;
        self.flush_memory_locked(&mut w)
    }

    /// Turns currently held only in the live journal (not yet compacted).
    pub fn live_turns(&self) -> Vec<Turn> {
        match &self.writer {
            Some(w) => w.lock().unwrap_or_else(|e| e.into_inner()).live.clone(),
            None => Vec::new(),
        }
    }

    fn refresh_live(&self, w: &mut Writer) -> Result<()> {
        w.live_handle = build_live(&w.live, &w.manifest.compile)?;
        let snap = self.snapshot();
        let next = assemble(
            &self.dir,
            &self.options,
            &w.manifest,
            Some(&snap),
            w.live_handle.clone(),
        )?;
        self.snapshot.store(Arc::new(next));
        Ok(())
    }

    pub(super) fn flush_memory_locked(&self, w: &mut Writer) -> Result<bool> {
        let Some(through) = w.live.last().map(|t| t.seq) else {
            return Ok(false);
        };
        let docs: Vec<SourceDoc> = w.live.iter().map(turn_doc).collect();
        let live = std::mem::take(&mut w.live);
        let handle = w.live_handle.take();
        let flushed = (|| {
            let mut staged = StagedWrite::new(self, w);
            for doc in docs {
                staged.push(doc)?;
            }
            staged.commit(Vec::new(), Some(through), &[])
        })();
        if let Err(e) = flushed {
            w.live = live;
            w.live_handle = handle;
            return Err(e);
        }
        // The manifest now records `through`; replay skips older records,
        // so a failed rotation only costs journal space.
        if let Err(e) = w.journal.rewrite(&self.dir, &[]) {
            log::warn!("memory journal rotation failed: {e}");
        }
        Ok(true)
    }
}
