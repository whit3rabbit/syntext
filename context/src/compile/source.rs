//! [`SourceDoc`]: a document handed to the store for compilation.

use syntext::changes::ContentDigest;

use crate::error::Result;
use crate::structure::Lang;
use crate::types::{DocKey, DocKind, Role, Scope, Trust};

/// A document to compile into a store.
///
/// Construct with one of the helpers ([`SourceDoc::file`],
/// [`SourceDoc::note`], [`SourceDoc::external`], [`SourceDoc::new`]) and
/// refine with the `with_*` methods. Upserting a document whose key already
/// exists replaces the previous version atomically.
#[derive(Debug, Clone)]
pub struct SourceDoc {
    /// Store-unique key.
    pub key: DocKey,
    /// Full UTF-8 text.
    pub text: String,
    /// Where the document came from.
    pub kind: DocKind,
    /// Source language; detected from `path` or content when `None`.
    pub lang: Option<Lang>,
    /// Repository-relative path with forward slashes (files only).
    pub path: Option<String>,
    /// Human-readable title (defaults to the path or key).
    pub title: Option<String>,
    /// Trust level of the origin.
    pub trust: Trust,
    /// Visibility scope.
    pub scope: Scope,
    /// Unix seconds (file mtime, message time).
    pub timestamp: Option<i64>,
    /// Ordering sequence (journal sequence for conversation turns).
    pub seq: u64,
    /// Grouping label (conversation session, source label).
    pub group: Option<String>,
    /// Speaker, for conversation turns.
    pub role: Option<Role>,
    /// Digest of the original source bytes, when they differ from `text`
    /// (BOM stripped, transcoded). Computed from `text` when `None`.
    pub source_hash: Option<ContentDigest>,
}

impl SourceDoc {
    /// A free-form note with the given key.
    pub fn new(key: DocKey, text: impl Into<String>) -> Self {
        SourceDoc {
            key,
            text: text.into(),
            kind: DocKind::Note,
            lang: None,
            path: None,
            title: None,
            trust: Trust::Workspace,
            scope: Scope::default(),
            timestamp: None,
            seq: 0,
            group: None,
            role: None,
            source_hash: None,
        }
    }

    /// A file under the labelled source root (`<label>:<rel_path>`).
    pub fn file(label: &str, rel_path: &str, text: impl Into<String>) -> Result<Self> {
        let key = DocKey::for_file(label, rel_path)?;
        let path = rel_path.replace('\\', "/");
        let mut doc = SourceDoc::new(key, text);
        doc.kind = DocKind::File;
        doc.lang = Lang::from_path(&path);
        doc.path = Some(path);
        doc.group = Some(label.to_string());
        Ok(doc)
    }

    /// A host-supplied note keyed `note:<id>`.
    pub fn note(id: &str, text: impl Into<String>) -> Result<Self> {
        Ok(SourceDoc::new(DocKey::new(format!("note:{id}"))?, text))
    }

    /// External content (web page, tool output) keyed `ext:<uri>`, with
    /// [`Trust::External`].
    pub fn external(uri: &str, text: impl Into<String>) -> Result<Self> {
        let mut doc = SourceDoc::new(DocKey::new(format!("ext:{uri}"))?, text);
        doc.kind = DocKind::External;
        doc.trust = Trust::External;
        Ok(doc)
    }

    /// Set the title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the trust level.
    pub fn with_trust(mut self, trust: Trust) -> Self {
        self.trust = trust;
        self
    }

    /// Set the scope.
    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// Set the language explicitly.
    pub fn with_lang(mut self, lang: Lang) -> Self {
        self.lang = Some(lang);
        self
    }

    /// Set the timestamp (unix seconds).
    pub fn with_timestamp(mut self, unix_secs: i64) -> Self {
        self.timestamp = Some(unix_secs);
        self
    }

    /// Set the document kind.
    pub fn with_kind(mut self, kind: DocKind) -> Self {
        self.kind = kind;
        self
    }

    /// Set the grouping label.
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// Set the digest of the original source bytes.
    pub fn with_source_hash(mut self, digest: ContentDigest) -> Self {
        self.source_hash = Some(digest);
        self
    }

    /// Display location used in breadcrumbs and receipts.
    pub(crate) fn location(&self) -> String {
        self.path
            .clone()
            .or_else(|| self.title.clone())
            .unwrap_or_else(|| self.key.as_str().to_string())
    }
}

/// Decode raw file bytes to text: strips a UTF-8 BOM, transcodes UTF-16
/// with a BOM, and replaces invalid UTF-8 sequences. Returns `None` for
/// binary content (a NUL byte in the first 8 KiB, the same rule syntext
/// uses), which is never compiled.
pub fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if syntext::index::is_binary(bytes) {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> Option<String> {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| f(c)).collect();
    let text = String::from_utf16_lossy(&units);
    (!text.chars().take(8192).any(|c| c == '\0')).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_kind_trust_and_keys() {
        let f = SourceDoc::file("repo", "src\\lib.rs", "fn a() {}").unwrap();
        assert_eq!(f.key.as_str(), "repo:src/lib.rs");
        assert_eq!(f.kind, DocKind::File);
        assert_eq!(f.lang, Some(Lang::Rust));
        assert_eq!(f.location(), "src/lib.rs");
        let e = SourceDoc::external("https://x.test/a", "t").unwrap();
        assert_eq!(e.trust, Trust::External);
        assert_eq!(e.key.as_str(), "ext:https://x.test/a");
        let n = SourceDoc::note("n1", "t").unwrap().with_title("Plan");
        assert_eq!(n.location(), "Plan");
    }

    #[test]
    fn decodes_boms_and_rejects_binary() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFhi").as_deref(), Some("hi"));
        assert_eq!(
            decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]).as_deref(),
            Some("hi")
        );
        assert_eq!(
            decode_text(&[0xFE, 0xFF, 0, b'h', 0, b'i']).as_deref(),
            Some("hi")
        );
        assert_eq!(decode_text(b"a\0b"), None);
        assert_eq!(
            decode_text(b"bad \xFF byte").as_deref(),
            Some("bad \u{FFFD} byte")
        );
    }
}
