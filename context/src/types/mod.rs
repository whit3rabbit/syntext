//! Identifiers and enumerations shared across the crate.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{ContextError, Result};

mod kinds;

pub use kinds::{DocKind, PassageKind, Role, SymbolKind, Trust};

/// Longest accepted [`DocKey`], in bytes.
pub const MAX_DOC_KEY_BYTES: usize = 1024;
/// Longest accepted [`Scope`] label, in bytes.
pub const MAX_SCOPE_BYTES: usize = 128;

/// Store-unique identity of a document, e.g. `repo:src/lib.rs` or `turn:42`.
///
/// Upserting a document with an existing key replaces the old version
/// atomically. Keys are opaque labels, never opened as filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DocKey(String);

impl DocKey {
    /// Validate and wrap a key. Rejects empty keys, keys longer than
    /// [`MAX_DOC_KEY_BYTES`], and keys containing control characters.
    pub fn new(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if key.is_empty() || key.len() > MAX_DOC_KEY_BYTES {
            return Err(ContextError::invalid(format!(
                "document key must be 1..={MAX_DOC_KEY_BYTES} bytes"
            )));
        }
        if key.chars().any(char::is_control) {
            return Err(ContextError::invalid(
                "document key contains control characters",
            ));
        }
        Ok(DocKey(key))
    }

    /// Key for a file under a labelled source root: `<label>:<relative path>`,
    /// with the path normalized to forward slashes.
    pub fn for_file(label: &str, rel_path: &str) -> Result<Self> {
        validate_label(label)?;
        let rel = rel_path.replace('\\', "/");
        if rel.is_empty() {
            return Err(ContextError::invalid("empty relative path"));
        }
        DocKey::new(format!("{label}:{rel}"))
    }

    /// The key as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DocKey {
    type Error = ContextError;
    fn try_from(value: String) -> Result<Self> {
        DocKey::new(value)
    }
}

impl From<DocKey> for String {
    fn from(key: DocKey) -> String {
        key.0
    }
}

impl fmt::Display for DocKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validate a source label (`[a-z0-9_-]+`, at most 64 bytes). Labels prefix
/// file keys and name per-source state directories, so they are restricted
/// to characters that are safe in a path component on every platform.
pub(crate) fn validate_label(label: &str) -> Result<()> {
    let ok = !label.is_empty()
        && label.len() <= 64
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(ContextError::invalid(format!(
            "source label {label:?} must match [a-z0-9_-]{{1,64}}"
        )))
    }
}

/// Visibility label carried by every document.
///
/// Retrieval only returns documents whose scope the request grants (see
/// [`ScopeFilter`]). Being able to name a store does not imply permission to
/// read every scope in it: memory written by one application stays invisible
/// to another unless the host grants that scope explicitly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Scope(String);

impl Scope {
    /// Label used for documents that were not given a scope.
    pub const DEFAULT: &'static str = "default";

    /// Validate and wrap a scope label: 1..=128 bytes of ASCII alphanumerics
    /// or `- _ . : / @`.
    pub fn new(label: impl Into<String>) -> Result<Self> {
        let label = label.into();
        let ok = !label.is_empty()
            && label.len() <= MAX_SCOPE_BYTES
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b));
        if ok {
            Ok(Scope(label))
        } else {
            Err(ContextError::invalid(format!(
                "invalid scope label {label:?}"
            )))
        }
    }

    /// The default scope.
    pub fn default_scope() -> Self {
        Scope(Self::DEFAULT.to_string())
    }

    /// Scope shared within one project or repository: `project:<id>`.
    pub fn project(id: &str) -> Result<Self> {
        Scope::new(format!("project:{id}"))
    }

    /// Scope private to one application: `app:<id>`.
    pub fn app(id: &str) -> Result<Self> {
        Scope::new(format!("app:{id}"))
    }

    /// The label as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Scope {
    fn default() -> Self {
        Scope::default_scope()
    }
}

impl TryFrom<String> for Scope {
    type Error = ContextError;
    fn try_from(value: String) -> Result<Self> {
        Scope::new(value)
    }
}

impl From<Scope> for String {
    fn from(scope: Scope) -> String {
        scope.0
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which scopes a retrieval request may read.
///
/// The default grants only [`Scope::DEFAULT`], so memory and other labelled
/// content must be granted explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeFilter {
    /// Every scope is visible. Intended for single-tenant hosts only.
    All,
    /// Only the listed scopes are visible.
    Only(BTreeSet<Scope>),
}

impl ScopeFilter {
    /// A filter granting exactly the given scopes.
    pub fn only(scopes: impl IntoIterator<Item = Scope>) -> Self {
        ScopeFilter::Only(scopes.into_iter().collect())
    }

    /// Return a filter that additionally grants `scope`.
    pub fn grant(self, scope: Scope) -> Self {
        match self {
            ScopeFilter::All => ScopeFilter::All,
            ScopeFilter::Only(mut set) => {
                set.insert(scope);
                ScopeFilter::Only(set)
            }
        }
    }

    /// Whether a document labelled `scope` is visible.
    pub fn allows(&self, scope: &str) -> bool {
        match self {
            ScopeFilter::All => true,
            ScopeFilter::Only(set) => set.iter().any(|s| s.as_str() == scope),
        }
    }
}

impl Default for ScopeFilter {
    fn default() -> Self {
        ScopeFilter::only([Scope::default_scope()])
    }
}

#[cfg(test)]
mod tests;
