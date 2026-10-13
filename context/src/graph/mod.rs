//! Entity graph: canonical entities, aliases, and typed relations.
//!
//! Every entity is identified by a canonical key ([`fold_key`]): lowercase
//! subwords joined by spaces, so `parse_query`, `parseQuery`, and "Parse
//! Query" are one entity without any alias bookkeeping. Explicit aliases
//! cover what folding cannot (an acronym and its expansion, `import numpy as
//! np`, "Northstar, formerly Polaris"). Relations carry a predicate, a kind,
//! a confidence, and the passage they were read from, so traversing an edge
//! always yields evidence the model can see.
//!
//! The graph is optional per store ([`GraphMode`]) and lives inside the
//! store's own segments: there is no separate graph database to run, and a
//! store compiled with `GraphMode::Off` simply has no graph signal.
//!
//! [`fold_key`]: crate::analysis::fold_key

mod code;
mod prose;
mod spans;

use serde::{Deserialize, Serialize};

use crate::structure::{Lang, PassageSpan};
use crate::types::PassageKind;

/// How much of the entity graph to extract at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GraphMode {
    /// No graph. Retrieval relies on lexical, exact-match, and structural
    /// signals only.
    Off,
    /// Code facts only: definitions, containment, calls, inheritance, and
    /// import aliases.
    Code,
    /// Code facts plus prose: named entities, acronym and "also known as"
    /// aliases, and relations read from sentences.
    #[default]
    Full,
}

/// What an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EntityKind {
    /// A code symbol (definition or identifier).
    Symbol,
    /// A named entity from prose (person, organization, product, ...).
    Name,
}

impl EntityKind {
    pub(crate) fn code(self) -> u8 {
        match self {
            EntityKind::Symbol => 0,
            EntityKind::Name => 1,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(EntityKind::Symbol),
            1 => Some(EntityKind::Name),
            _ => None,
        }
    }
}

/// Kind of relation, which sets traversal behaviour and default confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// A predicate read from a sentence ("works for", "acquired").
    Relation,
    /// Caller -> callee.
    Calls,
    /// Method or member -> enclosing type.
    MemberOf,
    /// Type -> implemented trait or interface.
    Implements,
    /// Type -> base type.
    Extends,
    /// Two entities named in one sentence with no recognized predicate.
    CoMention,
}

impl EdgeKind {
    pub(crate) fn code(self) -> u8 {
        match self {
            EdgeKind::Relation => 0,
            EdgeKind::Calls => 1,
            EdgeKind::MemberOf => 2,
            EdgeKind::Implements => 3,
            EdgeKind::Extends => 4,
            EdgeKind::CoMention => 5,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => EdgeKind::Relation,
            1 => EdgeKind::Calls,
            2 => EdgeKind::MemberOf,
            3 => EdgeKind::Implements,
            4 => EdgeKind::Extends,
            5 => EdgeKind::CoMention,
            _ => return None,
        })
    }
}

/// An entity occurrence: canonical key plus the surface form seen.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntityRef {
    pub key: String,
    pub display: String,
    pub kind: EntityKind,
}

/// `alias` refers to the same thing as `target` (both canonical keys).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AliasFact {
    pub alias: String,
    pub target: String,
    pub confidence: f32,
}

/// A directed relation between two canonical keys.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeFact {
    pub subject: EntityRef,
    pub predicate: String,
    pub object: EntityRef,
    pub kind: EdgeKind,
    pub confidence: f32,
}

/// Everything extracted from one passage.
#[derive(Debug, Default, Clone)]
pub(crate) struct PassageFacts {
    /// Entities this passage defines (code definitions).
    pub defines: Vec<EntityRef>,
    /// Named entities this passage mentions (prose).
    pub mentions: Vec<EntityRef>,
    pub aliases: Vec<AliasFact>,
    pub edges: Vec<EdgeFact>,
}

impl PassageFacts {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.defines.is_empty()
            && self.mentions.is_empty()
            && self.aliases.is_empty()
            && self.edges.is_empty()
    }
}

/// Longest canonical key kept, in bytes.
pub(crate) const MAX_KEY_BYTES: usize = 96;

/// Build an [`EntityRef`] from a surface form, or `None` when the folded key
/// is empty, oversized, or has no letters.
pub(crate) fn entity(display: &str, kind: EntityKind) -> Option<EntityRef> {
    let key = crate::analysis::fold_key(display);
    if key.is_empty() || key.len() > MAX_KEY_BYTES || !key.chars().any(char::is_alphabetic) {
        return None;
    }
    Some(EntityRef {
        key,
        display: display.trim().to_string(),
        kind,
    })
}

/// Extract graph facts from one passage of a document.
pub(crate) fn extract(text: &str, span: &PassageSpan, lang: Lang, mode: GraphMode) -> PassageFacts {
    let mut facts = PassageFacts::default();
    if mode == GraphMode::Off {
        return facts;
    }
    let body = &text[span.start..span.end];
    match span.kind {
        PassageKind::Code | PassageKind::Definition => code::extract(body, span, lang, &mut facts),
        PassageKind::Prose | PassageKind::Turn if mode == GraphMode::Full => {
            prose::extract(body, &mut facts)
        }
        _ => {}
    }
    dedup(&mut facts);
    facts
}

fn dedup(facts: &mut PassageFacts) {
    facts.defines.sort_by(|a, b| a.key.cmp(&b.key));
    facts.defines.dedup_by(|a, b| a.key == b.key);
    facts.mentions.sort_by(|a, b| a.key.cmp(&b.key));
    facts.mentions.dedup_by(|a, b| a.key == b.key);
    facts
        .aliases
        .sort_by(|a, b| (&a.alias, &a.target).cmp(&(&b.alias, &b.target)));
    facts
        .aliases
        .dedup_by(|a, b| a.alias == b.alias && a.target == b.target);
    facts.edges.sort_by(|a, b| {
        (&a.subject.key, &a.predicate, &a.object.key).cmp(&(
            &b.subject.key,
            &b.predicate,
            &b.object.key,
        ))
    });
    facts.edges.dedup_by(|a, b| {
        a.subject.key == b.subject.key && a.predicate == b.predicate && a.object.key == b.object.key
    });
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
