//! Directory sources: keep a store in sync with a directory tree.
//!
//! Discovery is syntext's own repository walk (`.gitignore`-aware, nested
//! checkouts pruned, symlink aliases deduplicated), so a context store sees
//! exactly the files a syntext index over the same root sees. Change
//! detection is syntext's change catalogue: a stat fast path plus BLAKE3
//! digests with git's racily-clean rule, so a sync re-reads only files whose
//! content actually changed and a merely touched file costs one hash.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use syntext::changes::ContentDigest;

use crate::error::Result;
use crate::types::{validate_label, Scope, Trust};

/// Consumer name under which syncs acknowledge catalogue generations.
pub(super) const CONSUMER: &str = "syntext-context";
/// Paths observed per catalogue pass. The catalogue keeps the bytes of the
/// small files it hashes, so observing in chunks bounds that memory too.
pub(super) const OBSERVE_CHUNK: usize = 512;

/// Extensions that are never text; skipped without reading.
const BINARY_EXTENSIONS: &[&str] = &[
    "7z", "a", "avif", "bin", "bmp", "class", "dll", "dylib", "eot", "exe", "gif", "gz", "ico",
    "jar", "jpeg", "jpg", "lib", "mp3", "mp4", "o", "obj", "otf", "pdf", "png", "pyc", "so", "tar",
    "tgz", "ttf", "wasm", "webm", "webp", "woff", "woff2", "xz", "zip", "zst",
];

/// A directory whose files are mirrored into a store under one label.
#[derive(Debug, Clone)]
pub struct DirectorySource {
    /// Label prefixing document keys (`<label>:<relative path>`).
    pub label: String,
    /// Root directory.
    pub root: PathBuf,
    /// Scope given to every document.
    pub scope: Scope,
    /// Trust given to every document.
    pub trust: Trust,
    /// Files larger than this are skipped.
    pub max_file_bytes: u64,
    /// Also index nested git checkouts (submodules, worktrees) below root.
    pub include_nested_checkouts: bool,
    /// Relative path prefixes (forward slashes) to leave out.
    pub exclude: Vec<String>,
}

impl DirectorySource {
    /// A source for `root` labelled `label` (`[a-z0-9_-]{1,64}`).
    pub fn new(label: &str, root: impl Into<PathBuf>) -> Result<Self> {
        validate_label(label)?;
        Ok(DirectorySource {
            label: label.to_string(),
            root: root.into(),
            scope: Scope::default(),
            trust: Trust::Workspace,
            max_file_bytes: 2 * 1024 * 1024,
            include_nested_checkouts: false,
            exclude: Vec::new(),
        })
    }

    /// Set the scope.
    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// Set the trust level.
    pub fn with_trust(mut self, trust: Trust) -> Self {
        self.trust = trust;
        self
    }

    /// Exclude a relative path prefix.
    pub fn exclude(mut self, prefix: &str) -> Self {
        self.exclude.push(prefix.trim_matches('/').to_string());
        self
    }
}

/// What a sync changed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    /// Store generation after the sync.
    pub generation: u64,
    /// Files discovered under the root.
    pub scanned: usize,
    /// New documents.
    pub added: usize,
    /// Changed documents.
    pub modified: usize,
    /// Documents removed (deleted, ignored, binary, or excluded).
    pub removed: usize,
    /// Files skipped as binary, oversized, or unreadable.
    pub skipped: usize,
    /// Paths whose state could not be observed (left as they were).
    pub uncertain: usize,
}

/// Repository-relative path as a forward-slash string; `None` for paths
/// that are not plain relative paths.
pub(super) fn rel_string(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Discovery filter: no VCS internals, no index directories, no binary
/// extensions, nothing under the store itself or an excluded prefix.
pub(super) fn keep(rel: &str, source: &DirectorySource, store_rel: Option<&Path>) -> bool {
    let in_internal_dir = rel
        .split('/')
        .any(|c| matches!(c, ".git" | ".hg" | ".svn" | ".syntext"));
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    let binary_ext = ext.is_some_and(|e| BINARY_EXTENSIONS.binary_search(&e.as_str()).is_ok());
    let in_store = store_rel
        .and_then(rel_string)
        .is_some_and(|s| rel == s || rel.starts_with(&format!("{s}/")));
    let excluded = source
        .exclude
        .iter()
        .any(|p| !p.is_empty() && (rel == p || rel.starts_with(&format!("{p}/"))));
    !(in_internal_dir || binary_ext || in_store || excluded)
}

pub(super) fn read_capped(path: &Path, max: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(max + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= max).then_some(buf)
}

#[cfg(test)]
pub(crate) fn binary_extensions() -> &'static [&'static str] {
    BINARY_EXTENSIONS
}

/// Digest helper shared with receipts: hash file bytes the way the
/// catalogue does.
pub(crate) fn file_digest(path: &Path) -> Option<ContentDigest> {
    let mut f = std::fs::File::open(path).ok()?;
    ContentDigest::from_reader(&mut f).ok()
}

#[cfg(test)]
#[path = "directory_tests.rs"]
mod tests;
