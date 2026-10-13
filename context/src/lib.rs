//! Retrieval-driven context engine for small-window local inference.
//!
//! `syntext-context` lets a model with a 4K-8K token window work over
//! repositories, document sets, and conversation histories far larger than
//! its window, without pretending the model reads all of it. Selection
//! happens outside the model, in deterministic Rust:
//!
//! ```text
//! COMPILE   sources -> structural passages -> lexical index + entity graph -> segments
//! RETRIEVE  question -> lexical + exact (syntext) + graph candidates -> rerank
//! ASSEMBLE  ranked passages -> token-budgeted evidence packet -> chat messages
//! PERSIST   conversation turns -> durable journal -> indexed memory
//! EXPLAIN   evidence packet -> receipt with source hashes, ranges, and signals
//! ```
//!
//! Only the selected evidence enters the prompt, and it enters as untrusted
//! reference data in a user message, never in the system role.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub(crate) mod analysis;
pub mod compile;
pub mod engine;
pub mod error;
pub mod graph;
pub mod memory;
pub mod pack;
pub mod receipt;
pub mod render;
pub mod retrieve;
pub mod store;
pub mod structure;
pub mod tokens;
pub mod types;

pub use compile::{
    decode_text, CompileOptions, DirectorySource, SkipReason, SourceDoc, SyncReport,
    COMPILER_VERSION,
};
pub use engine::{
    ChatRequest, ChatTurn, ContextEngine, EngineBuilder, EvidencePacket, PrepareRequest,
};
pub use error::{ContextError, Result};
pub use graph::{EdgeKind, GraphMode};
pub use memory::{ConversationMemory, MemoryMode};
pub use pack::{
    Budget, EvidenceBlock, EvidenceOrder, Excluded, ExclusionReason, PackOptions, MESSAGE_OVERHEAD,
};
pub use receipt::{EvidenceRef, Receipt, SourceCheck, SourceStatus, RECEIPT_VERSION};
pub use render::{
    ChatMessage, EvidencePlacement, RenderOptions, SanitizeOptions, EVIDENCE_POLICY,
    EVIDENCE_PREAMBLE,
};
pub use retrieve::{
    Chain, ChainStep, QueryIntent, QueryReport, RankWeights, RankedPassage, RetrieveOptions,
    Retrieved, Signals, StageTimings, SyntextSource,
};
pub use store::{
    CompactReport, ContextStore, DocSummary, IngestReport, StoreOptions, StoreSnapshot, StoreStats,
    Turn,
};
pub use structure::{Lang, SegmentOptions};
pub use tokens::{FnCounter, HeuristicCounter, TokenCounter};
pub use types::{
    DocKey, DocKind, PassageKind, Role, Scope, ScopeFilter, SymbolKind, Trust, MAX_DOC_KEY_BYTES,
    MAX_SCOPE_BYTES,
};
