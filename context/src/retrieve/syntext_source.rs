//! Exact-match evidence from a syntext index over the same files.
//!
//! Lexical ranking finds passages *about* a question; a syntext search finds
//! every line that literally contains an identifier from it, verified
//! against live file bytes. Each hit is mapped back onto the store:
//!
//! - if the store holds the file and its stored text is byte-identical to
//!   what syntext just verified, the hit marks that stored passage;
//! - otherwise (the file is new, or changed since the last sync) the live
//!   content is segmented on the spot and the enclosing structural unit
//!   becomes a candidate, so answers never lag behind the working tree.

use std::collections::HashMap;
use std::sync::Arc;

use syntext::changes::ContentDigest;
use syntext::index::Index;
use syntext::SearchOptions;

use super::access::LiveDoc;
use super::candidates::Visibility;
use super::query::AnalyzedQuery;
use crate::compile::CompileOptions;
use crate::error::Result;
use crate::store::{PassageLoc, StoreSnapshot};
use crate::structure::{segment, Lang};
use crate::types::{validate_label, DocKey, DocKind, Scope, ScopeFilter, Trust};

/// A syntext index whose files are mirrored in the store under `label`
/// (see [`DirectorySource`](crate::compile::DirectorySource)).
#[derive(Clone)]
pub struct SyntextSource {
    index: Arc<Index>,
    label: String,
    scope: Scope,
    trust: Trust,
    max_matches: usize,
    max_identifiers: usize,
}

impl std::fmt::Debug for SyntextSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntextSource")
            .field("label", &self.label)
            .finish()
    }
}

impl SyntextSource {
    /// Exact search over `index`, whose files are stored under `label`.
    pub fn new(index: Arc<Index>, label: &str) -> Result<Self> {
        validate_label(label)?;
        Ok(SyntextSource {
            index,
            label: label.to_string(),
            scope: Scope::default(),
            trust: Trust::Workspace,
            max_matches: 200,
            max_identifiers: 4,
        })
    }

    /// Scope of the indexed files (must match the directory source's).
    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// Trust of the indexed files.
    pub fn with_trust(mut self, trust: Trust) -> Self {
        self.trust = trust;
        self
    }

    /// Cap on matching lines read per identifier (default 200).
    pub fn with_max_matches(mut self, n: usize) -> Self {
        self.max_matches = n.max(1);
        self
    }

    /// The underlying index.
    pub fn index(&self) -> &Arc<Index> {
        &self.index
    }
}

/// A hit on a passage segmented from live content.
pub(crate) struct LiveHit {
    pub doc: Arc<LiveDoc>,
    pub passage: usize,
    pub lines: Vec<u32>,
}

#[derive(Default)]
pub(crate) struct ExactResults {
    pub stored: HashMap<PassageLoc, Vec<u32>>,
    pub live: Vec<LiveHit>,
}

impl SyntextSource {
    pub(crate) fn exact(
        &self,
        q: &AnalyzedQuery,
        snap: &StoreSnapshot,
        compile: &CompileOptions,
        vis: &Visibility,
        scopes: &ScopeFilter,
        exclude: &[DocKey],
    ) -> Result<ExactResults> {
        let mut out = ExactResults::default();
        if !scopes.allows(self.scope.as_str()) {
            return Ok(out);
        }
        let mut live_docs: HashMap<String, Arc<LiveDoc>> = HashMap::new();
        let mut live_hits: HashMap<(String, usize), Vec<u32>> = HashMap::new();
        for ident in q.identifiers.iter().take(self.max_identifiers) {
            let mut opts = SearchOptions::default();
            opts.max_results = Some(self.max_matches);
            opts.case_insensitive = true;
            for fm in self.index.search_grouped(&regex::escape(ident), &opts)? {
                let rel = fm.path.to_string_lossy().replace('\\', "/");
                let key = DocKey::for_file(&self.label, &rel)?;
                if exclude.contains(&key) {
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&fm.content) else {
                    continue;
                };
                let digest = ContentDigest::from_bytes(text.as_bytes()).to_hex();
                let stored = snap
                    .lookup(key.as_str())
                    .filter(|loc| snap.summary(*loc).text_hash == digest);
                if let Some(loc) = stored {
                    if !vis.doc(loc.seg, loc.doc) {
                        continue;
                    }
                    let r = snap.reader(loc.seg);
                    let d = &r.docs[loc.doc as usize];
                    let first = d.first_passage as usize;
                    let passages = &r.passages[first..first + d.passage_count as usize];
                    for m in &fm.matches {
                        let off = m.byte_offset as usize;
                        if let Some(i) = passages
                            .iter()
                            .position(|p| (p.start as usize..p.end as usize).contains(&off))
                        {
                            let loc = PassageLoc {
                                seg: loc.seg,
                                pid: (first + i) as u32,
                            };
                            out.stored.entry(loc).or_default().push(m.line_number);
                        }
                    }
                    continue;
                }
                let doc = live_docs
                    .entry(rel.clone())
                    .or_insert_with(|| Arc::new(self.live_doc(&key, &rel, text, &digest, compile)))
                    .clone();
                for m in &fm.matches {
                    let off = m.byte_offset as usize;
                    if let Some(i) = doc
                        .layout
                        .passages
                        .iter()
                        .position(|p| (p.start..p.end).contains(&off))
                    {
                        live_hits
                            .entry((rel.clone(), i))
                            .or_default()
                            .push(m.line_number);
                    }
                }
            }
        }
        let mut live: Vec<LiveHit> = live_hits
            .into_iter()
            .map(|((rel, passage), lines)| LiveHit {
                doc: Arc::clone(&live_docs[&rel]),
                passage,
                lines,
            })
            .collect();
        live.sort_by(|a, b| (&a.doc.key, a.passage).cmp(&(&b.doc.key, b.passage)));
        out.live = live;
        Ok(out)
    }

    fn live_doc(
        &self,
        key: &DocKey,
        rel: &str,
        text: &str,
        digest: &str,
        compile: &CompileOptions,
    ) -> LiveDoc {
        let lang = Lang::detect(Some(rel), text);
        let layout = segment(text, lang, DocKind::File, rel, &compile.segment);
        LiveDoc {
            key: key.as_str().to_string(),
            path: rel.to_string(),
            text: Arc::from(text),
            layout,
            scope: self.scope.as_str().to_string(),
            trust: self.trust,
            source_hash: digest.to_string(),
        }
    }
}
