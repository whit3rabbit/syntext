//! JSON shapes crossing the C ABI. Inputs ignore unknown fields; outputs
//! only ever gain fields. Field names are snake_case.

use serde::{Deserialize, Serialize};

use crate::compile::{DirectorySource, SourceDoc};
use crate::engine::EvidencePacket;
use crate::error::Result;
use crate::graph::GraphMode;
use crate::pack::{Budget, EvidenceBlock, EvidenceOrder};
use crate::receipt::Receipt;
use crate::render::{ChatMessage, EvidencePlacement};
use crate::retrieve::RankedPassage;
use crate::store::StoreOptions;
use crate::types::{DocKey, PassageKind, Scope, ScopeFilter, Trust};

/// `options_json` of `syntext_context_store_open`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct StoreOptionsDto {
    /// Open without the writer lock.
    pub read_only: bool,
    /// Fail instead of creating a store when none exists.
    pub must_exist: bool,
    pub sync: Option<bool>,
    pub batch_bytes: Option<usize>,
    pub max_segments: Option<usize>,
    pub max_dead_ratio: Option<f32>,
    pub memory_flush_turns: Option<usize>,
    pub verify_on_open: Option<bool>,
    /// Graph extraction for a new store: `"full"`, `"code"`, or `"off"`.
    pub graph: Option<GraphMode>,
}

impl StoreOptionsDto {
    pub(super) fn options(&self) -> StoreOptions {
        let d = StoreOptions::default();
        let mut o = StoreOptions {
            sync: self.sync.unwrap_or(d.sync),
            batch_bytes: self.batch_bytes.unwrap_or(d.batch_bytes),
            max_segments: self.max_segments.unwrap_or(d.max_segments),
            max_dead_ratio: self.max_dead_ratio.unwrap_or(d.max_dead_ratio),
            memory_flush_turns: self.memory_flush_turns.unwrap_or(d.memory_flush_turns),
            verify_on_open: self.verify_on_open.unwrap_or(d.verify_on_open),
            ..d
        };
        if let Some(g) = self.graph {
            o.compile.graph = g;
        }
        o
    }
}

/// A directory source.
#[derive(Debug, Deserialize)]
pub(super) struct SourceDto {
    pub label: String,
    pub root: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub trust: Option<Trust>,
    #[serde(default)]
    pub max_file_bytes: Option<u64>,
    #[serde(default)]
    pub include_nested_checkouts: bool,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl SourceDto {
    pub(super) fn source(&self) -> Result<DirectorySource> {
        let mut s = DirectorySource::new(&self.label, &self.root)?;
        if let Some(scope) = &self.scope {
            s = s.with_scope(Scope::new(scope.as_str())?);
        }
        if let Some(t) = self.trust {
            s = s.with_trust(t);
        }
        if let Some(m) = self.max_file_bytes {
            s.max_file_bytes = m;
        }
        s.include_nested_checkouts = self.include_nested_checkouts;
        for prefix in &self.exclude {
            s = s.exclude(prefix);
        }
        Ok(s)
    }
}

/// A host-supplied document for `syntext_context_store_upsert`.
#[derive(Debug, Deserialize)]
pub(super) struct DocDto {
    /// Note id (key `note:<id>`), or with `external`, a URI (`ext:<uri>`).
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub external: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub trust: Option<Trust>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub timestamp: Option<i64>,
}

impl DocDto {
    pub(super) fn doc(self) -> Result<SourceDoc> {
        let mut d = if self.external {
            SourceDoc::external(&self.id, self.text)?
        } else {
            SourceDoc::note(&self.id, self.text)?
        };
        if let Some(t) = self.title {
            d = d.with_title(t);
        }
        if let Some(t) = self.trust {
            d = d.with_trust(t);
        }
        if let Some(s) = self.scope {
            d = d.with_scope(Scope::new(s)?);
        }
        if let Some(ts) = self.timestamp {
            d = d.with_timestamp(ts);
        }
        Ok(d)
    }
}

/// `options_json` of `syntext_context_engine_new`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct EngineOptionsDto {
    /// Model name recorded in receipts.
    pub model: Option<String>,
    /// Tokenizer name recorded in receipts when a count callback is given.
    pub tokenizer: Option<String>,
    /// Exact identifier search over a syntext index of the same files.
    pub syntext: Option<SyntextDto>,
    pub hops: Option<u8>,
    pub max_results: Option<usize>,
    pub max_blocks: Option<usize>,
    pub per_doc_cap: Option<usize>,
    pub relevance_floor: Option<f32>,
    pub expand: Option<bool>,
    pub order: Option<EvidenceOrder>,
    pub placement: Option<EvidencePlacement>,
    /// Append the evidence policy to the system prompt on turns with
    /// evidence (default true).
    pub append_policy: Option<bool>,
}

/// A syntext index whose files are mirrored under `label`.
#[derive(Debug, Deserialize)]
pub(super) struct SyntextDto {
    pub index_dir: String,
    pub repo_root: String,
    pub label: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub trust: Option<Trust>,
}

/// Scopes and exclusions shared by every query request.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct Filter {
    /// Scopes to read; absent means the engine default (`default` only).
    pub scopes: Option<Vec<String>>,
    /// Document keys to leave out.
    pub exclude: Vec<String>,
}

impl Filter {
    pub(super) fn scopes(&self) -> Result<Option<ScopeFilter>> {
        self.scopes
            .as_ref()
            .map(|v| {
                v.iter()
                    .map(|s| Scope::new(s.as_str()))
                    .collect::<Result<Vec<_>>>()
                    .map(ScopeFilter::only)
            })
            .transpose()
    }

    pub(super) fn exclude(&self) -> Result<Vec<DocKey>> {
        self.exclude
            .iter()
            .map(|k| DocKey::new(k.as_str()))
            .collect()
    }
}

/// `request_json` of `syntext_context_prepare`.
#[derive(Debug, Deserialize)]
pub(super) struct PrepareDto {
    pub question: String,
    pub evidence_tokens: usize,
    /// Turns of the memory handle already verbatim in the host's prompt.
    #[serde(default)]
    pub kept_turns: usize,
    #[serde(flatten)]
    pub filter: Filter,
}

/// `request_json` of `syntext_context_assemble`.
#[derive(Debug, Deserialize)]
pub(super) struct AssembleDto {
    pub question: String,
    pub window: usize,
    #[serde(default)]
    pub system: Option<String>,
    /// Prior messages; ignored when a memory handle is passed.
    #[serde(default)]
    pub history: Vec<ChatMessage>,
    #[serde(default)]
    pub generation_reserve: Option<usize>,
    #[serde(default)]
    pub history_share: Option<f32>,
    #[serde(flatten)]
    pub filter: Filter,
}

/// `request_json` of `syntext_context_search`.
#[derive(Debug, Deserialize)]
pub(super) struct SearchDto {
    pub question: String,
    #[serde(default)]
    pub max_results: Option<usize>,
    #[serde(flatten)]
    pub filter: Filter,
}

/// An evidence packet.
#[derive(Serialize)]
pub(super) struct PacketDto<'a> {
    pub blocks: &'a [EvidenceBlock],
    pub evidence_text: &'a str,
    pub evidence_tokens: usize,
    pub budget: usize,
    /// Least-trusted source in the packet (null when empty).
    pub lowest_trust: Option<Trust>,
    pub receipt: &'a Receipt,
    /// The same receipt as a JSON string, ready to store with the answer.
    pub receipt_json: String,
}

impl<'a> PacketDto<'a> {
    pub(super) fn new(p: &'a EvidencePacket) -> Self {
        PacketDto {
            blocks: &p.blocks,
            evidence_text: &p.evidence_text,
            evidence_tokens: p.evidence_tokens,
            budget: p.budget,
            lowest_trust: p.lowest_trust(),
            receipt: &p.receipt,
            receipt_json: p.receipt.to_json(),
        }
    }
}

/// An assembled prompt.
#[derive(Serialize)]
pub(super) struct TurnDto<'a> {
    pub messages: &'a [ChatMessage],
    pub packet: PacketDto<'a>,
    pub budget: &'a Budget,
    pub history_kept: usize,
    pub history_dropped: usize,
    pub prompt_tokens: usize,
}

/// One ranked passage, for search tools.
#[derive(Serialize)]
pub(super) struct PassageDto<'a> {
    pub doc_key: &'a str,
    /// Path for files, title otherwise.
    pub source: &'a str,
    pub breadcrumb: &'a str,
    pub symbol: Option<&'a str>,
    pub kind: PassageKind,
    pub trust: Trust,
    pub scope: &'a str,
    pub lines: (u32, u32),
    pub score: f32,
    pub text: &'a str,
    /// Read from the live file rather than the store.
    pub live: bool,
}

impl<'a> PassageDto<'a> {
    pub(super) fn new(p: &'a RankedPassage) -> Self {
        PassageDto {
            doc_key: &p.doc_key,
            source: p.path.as_deref().unwrap_or(&p.title),
            breadcrumb: &p.breadcrumb,
            symbol: p.symbol.as_deref(),
            kind: p.kind,
            trust: p.trust,
            scope: &p.scope,
            lines: p.lines,
            score: p.score,
            text: &p.text,
            live: p.live,
        }
    }
}
