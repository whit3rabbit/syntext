//! Request and result types of the engine.

use crate::memory::ConversationMemory;
use crate::pack::{Budget, EvidenceBlock};
use crate::receipt::Receipt;
use crate::render::{messages, ChatMessage, RenderOptions};
use crate::types::{DocKey, ScopeFilter, Trust};

/// Evidence chosen for one question, ready to place in a prompt.
#[derive(Debug, Clone)]
pub struct EvidencePacket {
    /// Blocks in prompt order.
    pub blocks: Vec<EvidenceBlock>,
    /// Rendered evidence section (empty when there is no evidence).
    pub evidence_text: String,
    /// Tokens used by `evidence_text`.
    pub evidence_tokens: usize,
    /// Tokens that were allowed.
    pub budget: usize,
    /// Provenance record.
    pub receipt: Receipt,
    pub(super) render: RenderOptions,
}

impl EvidencePacket {
    /// True when no evidence was selected.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Least-trusted source in the packet. Hosts can gate tools on it, e.g.
    /// disable side-effecting tools when external content is present.
    pub fn lowest_trust(&self) -> Option<Trust> {
        self.blocks.iter().map(|b| b.trust).min()
    }

    /// Chat messages: optional system prompt (plus evidence policy),
    /// `history`, then the evidence and the question in a user message.
    pub fn messages(
        &self,
        system: Option<&str>,
        history: &[ChatMessage],
        question: &str,
    ) -> Vec<ChatMessage> {
        messages(&self.evidence_text, system, history, question, &self.render)
    }
}

/// A request for evidence under an explicit token budget, for hosts that
/// build the prompt themselves.
#[derive(Debug, Clone)]
pub struct PrepareRequest<'a> {
    /// The question.
    pub question: &'a str,
    /// Tokens available for evidence.
    pub evidence_tokens: usize,
    /// Scopes to read (defaults to the engine's retrieval options).
    pub scopes: Option<ScopeFilter>,
    /// Documents to leave out.
    pub exclude: Vec<DocKey>,
    /// Conversation memory to recall older turns from: its scope is
    /// granted, and its `kept_turns` most recent turns are left out.
    pub memory: Option<&'a ConversationMemory>,
    /// How many of `memory`'s most recent turns the host already has
    /// verbatim in its prompt (so they are not repeated as evidence).
    pub kept_turns: usize,
}

impl<'a> PrepareRequest<'a> {
    /// Evidence for `question` within `evidence_tokens`.
    pub fn new(question: &'a str, evidence_tokens: usize) -> Self {
        PrepareRequest {
            question,
            evidence_tokens,
            scopes: None,
            exclude: Vec::new(),
            memory: None,
            kept_turns: 0,
        }
    }

    /// Recall older turns of `memory` as evidence. The host keeps its
    /// `kept_turns` most recent turns verbatim in its own prompt; those are
    /// left out of the evidence.
    pub fn memory(mut self, memory: &'a ConversationMemory, kept_turns: usize) -> Self {
        self.memory = Some(memory);
        self.kept_turns = kept_turns;
        self
    }
}

/// A chat turn to assemble for a model with a `window`-token context.
#[derive(Debug, Clone)]
pub struct ChatRequest<'a> {
    /// The new user question.
    pub question: &'a str,
    /// Model context window in tokens.
    pub window: usize,
    /// Host system prompt.
    pub system: Option<&'a str>,
    /// Prior messages (ignored when `memory` is set).
    pub history: &'a [ChatMessage],
    /// Conversation memory supplying history and memory scope.
    pub memory: Option<&'a ConversationMemory>,
    /// Tokens reserved for the answer (default: see [`Budget::for_window`]).
    pub generation_reserve: Option<usize>,
    /// Largest share of the window spent on verbatim history.
    pub history_share: f32,
    /// Scopes to read (the memory scope is added automatically).
    pub scopes: Option<ScopeFilter>,
    /// Documents to leave out.
    pub exclude: Vec<DocKey>,
}

impl<'a> ChatRequest<'a> {
    /// A request for `question` with a `window`-token model.
    pub fn new(question: &'a str, window: usize) -> Self {
        ChatRequest {
            question,
            window,
            system: None,
            history: &[],
            memory: None,
            generation_reserve: None,
            history_share: 0.3,
            scopes: None,
            exclude: Vec::new(),
        }
    }

    /// Set the system prompt.
    pub fn system(mut self, system: &'a str) -> Self {
        self.system = Some(system);
        self
    }

    /// Set explicit history.
    pub fn history(mut self, history: &'a [ChatMessage]) -> Self {
        self.history = history;
        self
    }

    /// Use conversation memory for history and recall.
    pub fn memory(mut self, memory: &'a ConversationMemory) -> Self {
        self.memory = Some(memory);
        self
    }
}

/// An assembled prompt.
#[derive(Debug, Clone)]
pub struct ChatTurn {
    /// Messages to send to the model.
    pub messages: Vec<ChatMessage>,
    /// The evidence included.
    pub packet: EvidencePacket,
    /// How the window was spent.
    pub budget: Budget,
    /// History messages kept verbatim.
    pub history_kept: usize,
    /// Older history messages left out of the prompt (still retrievable
    /// through memory).
    pub history_dropped: usize,
    /// Estimated prompt tokens, including template overhead.
    pub prompt_tokens: usize,
}
