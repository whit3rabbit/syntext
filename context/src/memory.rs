//! Conversation memory: recent turns verbatim in the prompt, older turns
//! searchable as evidence.
//!
//! A small window cannot hold a long conversation, and dropping old turns
//! loses what the user said. Every turn is journaled durably in the store
//! (see [`ContextStore::append_turn`]); [`ContextEngine::assemble`] keeps as
//! many recent turns verbatim as the history share of the window allows and
//! excludes exactly those from retrieval, so older turns come back only
//! when the question makes them relevant, as cited evidence.
//!
//! Memory is scoped. [`MemoryMode::Project`] and [`MemoryMode::App`] label
//! turns `project:<id>` or `app:<id>`, and a retrieval sees them only when
//! its scope filter grants that label, so two applications sharing a store
//! do not read each other's conversations by default.
//!
//! [`ContextEngine::assemble`]: crate::engine::ContextEngine::assemble

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::render::ChatMessage;
use crate::store::{turn_key, ContextStore};
use crate::types::{DocKey, DocKind, Role, Scope};

/// Whether and where conversation turns persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    /// Nothing persists; history lives only in this process.
    Off,
    /// Shared across sessions of one project (`project:<id>`).
    Project(String),
    /// Private to one application (`app:<id>`).
    App(String),
}

impl MemoryMode {
    /// The scope label turns are written under (`None` for `Off`).
    pub fn scope(&self) -> Result<Option<Scope>> {
        match self {
            MemoryMode::Off => Ok(None),
            MemoryMode::Project(id) => Scope::project(id).map(Some),
            MemoryMode::App(id) => Scope::app(id).map(Some),
        }
    }
}

#[derive(Debug, Clone)]
struct Recorded {
    message: ChatMessage,
    seq: Option<u64>,
}

/// One conversation session's memory.
pub struct ConversationMemory {
    store: Arc<ContextStore>,
    session: String,
    scope: Option<Scope>,
    turns: Mutex<Vec<Recorded>>,
}

impl std::fmt::Debug for ConversationMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationMemory")
            .field("session", &self.session)
            .field("scope", &self.scope)
            .finish()
    }
}

impl ConversationMemory {
    /// Start a new session.
    pub fn new(store: Arc<ContextStore>, session: &str, mode: MemoryMode) -> Result<Self> {
        Ok(ConversationMemory {
            store,
            session: session.to_string(),
            scope: mode.scope()?,
            turns: Mutex::new(Vec::new()),
        })
    }

    /// Resume a session: reload its persisted turns in order.
    pub fn resume(store: Arc<ContextStore>, session: &str, mode: MemoryMode) -> Result<Self> {
        let memory = Self::new(store, session, mode)?;
        let Some(scope) = memory.scope.clone() else {
            return Ok(memory);
        };
        let snap = memory.store.snapshot();
        let mut docs: Vec<_> = snap
            .documents()
            .into_iter()
            .filter(|d| {
                d.kind == DocKind::Turn
                    && d.group.as_deref() == Some(session)
                    && d.scope == scope.as_str()
            })
            .collect();
        docs.sort_by_key(|d| d.seq);
        let mut turns = Vec::with_capacity(docs.len());
        for d in docs {
            let text = snap.document_text(&d.key)?.unwrap_or_default();
            turns.push(Recorded {
                message: ChatMessage {
                    role: d.role.unwrap_or(Role::User),
                    content: text,
                },
                seq: Some(d.seq),
            });
        }
        *memory.turns.lock().unwrap_or_else(|e| e.into_inner()) = turns;
        Ok(memory)
    }

    /// Record one turn (durably, unless the mode is `Off`).
    pub fn record(&self, role: Role, text: &str) -> Result<()> {
        let seq = match &self.scope {
            Some(scope) => Some(
                self.store
                    .append_turn(&self.session, role, scope.clone(), text, None)?
                    .seq,
            ),
            None => None,
        };
        self.turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Recorded {
                message: ChatMessage {
                    role,
                    content: text.to_string(),
                },
                seq,
            });
        Ok(())
    }

    /// Record a user question and the model's answer.
    pub fn record_exchange(&self, question: &str, answer: &str) -> Result<()> {
        self.record(Role::User, question)?;
        self.record(Role::Assistant, answer)
    }

    /// Delete this session's turns from the store and from this handle, for
    /// a conversation the user deleted or rewrote (re-record the kept turns
    /// afterwards). Returns how many persisted turns were removed; with the
    /// mode `Off` only the in-process history is cleared. Compacted turns
    /// leave the disk at the next [`ContextStore::compact`]. Other handles
    /// open on the same session keep their in-process history; resume them.
    pub fn forget(&self) -> Result<usize> {
        let removed = match &self.scope {
            Some(scope) => self.store.forget_session(&self.session, scope)?,
            None => 0,
        };
        self.turns.lock().unwrap_or_else(|e| e.into_inner()).clear();
        Ok(removed)
    }

    /// Every turn of this session, oldest first.
    pub fn history(&self) -> Vec<ChatMessage> {
        self.turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| r.message.clone())
            .collect()
    }

    /// Session id.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Scope turns are written under (`None` when memory is off).
    pub fn scope(&self) -> Option<&Scope> {
        self.scope.as_ref()
    }

    /// Document keys of the `n` most recent persisted turns.
    pub(crate) fn recent_keys(&self, n: usize) -> Vec<DocKey> {
        let turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        turns
            .iter()
            .rev()
            .take(n)
            .filter_map(|r| r.seq.map(turn_key))
            .collect()
    }
}
