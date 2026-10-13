//! [`ContextEngine`]: the facade an inference host talks to.
//!
//! ```no_run
//! use std::sync::Arc;
//! use syntext_context::{
//!     ChatRequest, ContextEngine, ContextStore, DirectorySource, StoreOptions,
//! };
//!
//! let store = Arc::new(ContextStore::open_or_create(".context", StoreOptions::default())?);
//! store.sync_directory(&DirectorySource::new("repo", ".")?)?;
//! let engine = ContextEngine::builder(Arc::clone(&store)).build();
//!
//! // One call: evidence chosen and packed for an 8K window.
//! let turn = engine.assemble(&ChatRequest::new("Where is the query router?", 8192))?;
//! for m in &turn.messages {
//!     println!("{}: {} chars", m.role.as_str(), m.content.len());
//! }
//! println!("{}", turn.packet.receipt.to_json());
//! # Ok::<(), syntext_context::ContextError>(())
//! ```

mod request;

use std::sync::Arc;
use std::time::Instant;

pub use request::{ChatRequest, ChatTurn, EvidencePacket, PrepareRequest};

use crate::compile::CompileOptions;
use crate::embed::Embedder;
use crate::error::{ContextError, Result};
use crate::pack::{pack, Budget, PackOptions, MESSAGE_OVERHEAD};
use crate::receipt::{EvidenceRef, Receipt, RECEIPT_VERSION};
use crate::render::{ChatMessage, RenderOptions};
use crate::retrieve::{retrieve, RetrieveOptions, Retrieved, SyntextSource};
use crate::store::ContextStore;
use crate::tokens::{HeuristicCounter, TokenCounter};
use crate::types::Role;

/// Builder for [`ContextEngine`].
pub struct EngineBuilder {
    store: Arc<ContextStore>,
    syntext: Option<SyntextSource>,
    embedder: Option<Arc<dyn Embedder>>,
    counter: Arc<dyn TokenCounter>,
    retrieve: RetrieveOptions,
    pack: PackOptions,
    render: RenderOptions,
    model: Option<String>,
}

impl EngineBuilder {
    /// Add exact-match evidence from a syntext index.
    pub fn syntext(mut self, source: SyntextSource) -> Self {
        self.syntext = Some(source);
        self
    }

    /// Add semantic retrieval with the host's embedding model. Passages
    /// are compared once [`ContextStore::embed`] has given them vectors
    /// with the same embedder.
    pub fn embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Count tokens with the model's tokenizer (strongly recommended).
    pub fn token_counter(mut self, counter: Arc<dyn TokenCounter>) -> Self {
        self.counter = counter;
        self
    }

    /// Retrieval options.
    pub fn retrieve_options(mut self, opts: RetrieveOptions) -> Self {
        self.retrieve = opts;
        self
    }

    /// Packing options.
    pub fn pack_options(mut self, opts: PackOptions) -> Self {
        self.pack = opts;
        self
    }

    /// Rendering options.
    pub fn render_options(mut self, opts: RenderOptions) -> Self {
        self.render = opts;
        self
    }

    /// Model name recorded in receipts.
    pub fn model(mut self, name: impl Into<String>) -> Self {
        self.model = Some(name.into());
        self
    }

    /// Build the engine.
    pub fn build(self) -> ContextEngine {
        let compile = self.store.compile_options();
        ContextEngine {
            store: self.store,
            syntext: self.syntext,
            embedder: self.embedder,
            counter: self.counter,
            retrieve: self.retrieve,
            pack: self.pack,
            render: self.render,
            model: self.model,
            compile,
        }
    }
}

/// Retrieval, packing, and prompt assembly over one store.
pub struct ContextEngine {
    store: Arc<ContextStore>,
    syntext: Option<SyntextSource>,
    embedder: Option<Arc<dyn Embedder>>,
    counter: Arc<dyn TokenCounter>,
    retrieve: RetrieveOptions,
    pack: PackOptions,
    render: RenderOptions,
    model: Option<String>,
    compile: CompileOptions,
}

impl ContextEngine {
    /// Start building an engine over `store`.
    pub fn builder(store: Arc<ContextStore>) -> EngineBuilder {
        EngineBuilder {
            store,
            syntext: None,
            embedder: None,
            counter: Arc::new(HeuristicCounter::new()),
            retrieve: RetrieveOptions::default(),
            pack: PackOptions::default(),
            render: RenderOptions::default(),
            model: None,
        }
    }

    /// The underlying store.
    pub fn store(&self) -> &Arc<ContextStore> {
        &self.store
    }

    /// Retrieval options the engine was built with.
    pub fn retrieve_options(&self) -> &RetrieveOptions {
        &self.retrieve
    }

    /// Ranked passages for `question`, without packing.
    pub fn retrieve(&self, question: &str, opts: &RetrieveOptions) -> Result<Retrieved> {
        let snap = self.store.snapshot();
        let semantic = self.embedder.as_deref().map(|e| {
            let tables = self.store.vectors.tables(self.store.dir(), &snap, e.id());
            (e, tables)
        });
        retrieve(
            &snap,
            self.syntext.as_ref(),
            semantic,
            &self.compile,
            question,
            opts,
        )
    }

    /// The embedder used for semantic retrieval, if any.
    pub fn embedder(&self) -> Option<&Arc<dyn Embedder>> {
        self.embedder.as_ref()
    }

    /// Retrieve and pack evidence for `req.question` within its budget.
    pub fn prepare(&self, req: &PrepareRequest<'_>) -> Result<EvidencePacket> {
        let start = Instant::now();
        let mut opts = self.retrieve.clone();
        if let Some(s) = &req.scopes {
            opts.scopes = s.clone();
        }
        if let Some(m) = req.memory {
            if let Some(scope) = m.scope() {
                opts.scopes = opts.scopes.grant(scope.clone());
            }
            opts.exclude.extend(m.recent_keys(req.kept_turns));
        }
        opts.exclude.extend(req.exclude.iter().cloned());
        let retrieved = self.retrieve(req.question, &opts)?;
        let t = Instant::now();
        let packed = pack(
            &retrieved.passages,
            req.evidence_tokens,
            &*self.counter,
            &self.pack,
            &self.render,
        )?;
        let mut timings = retrieved.timings;
        timings.pack_us = t.elapsed().as_micros() as u64;
        timings.total_us = start.elapsed().as_micros() as u64;
        let receipt = Receipt {
            version: RECEIPT_VERSION,
            created_unix: crate::store::manifest::unix_now(),
            store_id: retrieved.store_id,
            generation: retrieved.generation,
            question: req.question.to_string(),
            intent: retrieved.query.intent,
            terms: retrieved.query.terms,
            identifiers: retrieved.query.identifiers,
            entities: retrieved.query.entities,
            evidence: packed.blocks.iter().map(EvidenceRef::from).collect(),
            excluded: packed.excluded,
            evidence_budget: req.evidence_tokens,
            evidence_tokens: packed.tokens,
            candidates_considered: retrieved.candidates_considered,
            timings,
            tokenizer: self.counter.name().to_string(),
            model: self.model.clone(),
            embedder: self.embedder.as_ref().map(|e| e.id().to_string()),
        };
        Ok(EvidencePacket {
            blocks: packed.blocks,
            evidence_text: packed.evidence_text,
            evidence_tokens: packed.tokens,
            budget: req.evidence_tokens,
            receipt,
            render: self.render.clone(),
        })
    }

    /// Assemble a complete prompt for a model with `req.window` tokens:
    /// keep recent history verbatim (up to `history_share`), spend what
    /// remains after the answer reserve on evidence, and verify the total
    /// with [`TokenCounter::count_messages`]. When it does not fit, evidence
    /// shrinks first, then the oldest history goes. If the system prompt and
    /// question alone exceed the window, the result is
    /// [`ContextError::WindowExceeded`]; an oversized prompt is never
    /// returned.
    pub fn assemble(&self, req: &ChatRequest<'_>) -> Result<ChatTurn> {
        let counter = &*self.counter;
        let history: Vec<ChatMessage> = match req.memory {
            Some(m) => m.history(),
            None => req.history.to_vec(),
        };
        let cap = (req.window as f32 * req.history_share.clamp(0.0, 1.0)) as usize;
        let mut kept = fit_history(&history, counter, cap);
        let mut base = Budget::for_window(req.window);
        if let Some(g) = req.generation_reserve {
            base.generation = g;
        }
        let limit = req.window.saturating_sub(base.generation);
        loop {
            let budget = base.measure(counter, req.system, kept, req.question);
            let mut evidence_tokens = budget.evidence_tokens();
            let needed = loop {
                let packet = self.prepare(&PrepareRequest {
                    question: req.question,
                    evidence_tokens,
                    scopes: req.scopes.clone(),
                    exclude: req.exclude.clone(),
                    memory: req.memory,
                    kept_turns: kept.len(),
                })?;
                let messages = packet.messages(req.system, kept, req.question);
                let prompt_tokens = counter.count_messages(&messages);
                if prompt_tokens <= limit {
                    return Ok(ChatTurn {
                        messages,
                        packet,
                        budget,
                        history_kept: kept.len(),
                        history_dropped: history.len() - kept.len(),
                        prompt_tokens,
                    });
                }
                if packet.is_empty() {
                    break prompt_tokens;
                }
                // Tokenizer non-additivity or the evidence policy overflowed.
                let over = prompt_tokens - limit;
                evidence_tokens = evidence_tokens.saturating_sub(over.max(16));
            };
            if kept.is_empty() {
                return Err(ContextError::WindowExceeded { needed, limit });
            }
            kept = drop_oldest(kept);
        }
    }
}

/// `history` without its oldest message, still starting with a user turn.
fn drop_oldest(history: &[ChatMessage]) -> &[ChatMessage] {
    let mut start = 1.min(history.len());
    while start < history.len() && history[start].role == Role::Assistant {
        start += 1;
    }
    &history[start..]
}

/// The longest suffix of `history` within `cap` tokens that does not begin
/// with an assistant message (templates expect a user turn first).
fn fit_history<'h>(
    history: &'h [ChatMessage],
    counter: &dyn TokenCounter,
    cap: usize,
) -> &'h [ChatMessage] {
    let mut used = 0;
    let mut start = history.len();
    for (i, m) in history.iter().enumerate().rev() {
        let cost = counter.count(&m.content) + MESSAGE_OVERHEAD;
        if used + cost > cap {
            break;
        }
        used += cost;
        start = i;
    }
    while start < history.len() && history[start].role == Role::Assistant {
        start += 1;
    }
    &history[start..]
}

#[cfg(test)]
mod tests;
