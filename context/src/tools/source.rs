//! The documents a tool call may name, and how a name the model wrote
//! resolves to one of them.

use crate::render::sanitize::attr;
use crate::store::{DocLoc, StoreSnapshot};
use crate::types::{DocKind, ScopeFilter, Trust};

/// One document the tools may read.
pub(super) struct DocInfo {
    pub loc: DocLoc,
    pub key: String,
    /// What evidence shows as the source: the path for files, else the
    /// title, else the key. Breadcrumbs start with it.
    pub source: String,
    pub path: Option<String>,
    pub kind: DocKind,
    pub trust: Trust,
    /// Lines in the document (the last passage's last line).
    pub lines: u32,
}

/// Live documents whose scope `scopes` grants, sorted by key. Conversation
/// turns are memory rather than corpus and are left out unless `turns`.
pub(super) fn visible(snap: &StoreSnapshot, scopes: &ScopeFilter, turns: bool) -> Vec<DocInfo> {
    let mut out: Vec<DocInfo> = snap
        .doc_index
        .iter()
        .filter_map(|(key, &loc)| {
            let r = snap.reader(loc.seg);
            let d = r.docs.get(loc.doc as usize)?;
            if !scopes.allows(r.str(d.scope)) || (d.kind == DocKind::Turn && !turns) {
                return None;
            }
            let path = Some(r.str(d.path)).filter(|p| !p.is_empty());
            let title = r.str(d.title);
            let source = match path {
                Some(p) => p,
                None if !title.is_empty() => title,
                None => key.as_str(),
            };
            let lines = d
                .passage_count
                .checked_sub(1)
                .and_then(|last| r.passages.get((d.first_passage + last) as usize))
                .map_or(0, |p| p.line_end);
            Some(DocInfo {
                loc,
                key: key.clone(),
                source: source.to_string(),
                path: path.map(str::to_string),
                kind: d.kind,
                trust: d.trust,
                lines,
            })
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// What a name resolved to.
pub(super) enum Resolved<'a> {
    Doc(&'a DocInfo),
    /// A directory, as a prefix ending in `/`.
    Dir(String),
    Ambiguous(Vec<&'a DocInfo>),
    /// Nothing; the documents whose names are closest.
    Missing(Vec<&'a DocInfo>),
}

/// A name as the model might write it: backslashes, `./`, or a leading `/`.
pub(super) fn normalize(name: &str) -> String {
    let mut s = name.trim().replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s.trim_start_matches('/').to_string()
}

/// Resolve `name` against `docs`: a key, a source exactly or ignoring case,
/// a directory (when `dirs`), or the tail of a path.
pub(super) fn resolve<'a>(docs: &'a [DocInfo], name: &str, dirs: bool) -> Resolved<'a> {
    let name = normalize(name);
    if let Some(d) = docs.iter().find(|d| d.key == name) {
        return Resolved::Doc(d);
    }
    let pick = |found: Vec<&'a DocInfo>| match found.len() {
        0 => None,
        1 => Some(Resolved::Doc(found[0])),
        _ => Some(Resolved::Ambiguous(found)),
    };
    let exact = docs.iter().filter(|d| d.source == name).collect();
    if let Some(r) = pick(exact) {
        return r;
    }
    let lower = name.to_lowercase();
    let folded = docs
        .iter()
        .filter(|d| d.source.to_lowercase() == lower)
        .collect();
    if let Some(r) = pick(folded) {
        return r;
    }
    if dirs {
        let prefix = if name.ends_with('/') {
            name.clone()
        } else {
            format!("{name}/")
        };
        if docs.iter().any(|d| in_dir(d, &prefix)) {
            return Resolved::Dir(prefix);
        }
    }
    let tail = format!("/{}", name.trim_end_matches('/'));
    let tails = docs
        .iter()
        .filter(|d| d.path.as_deref().is_some_and(|p| p.ends_with(&tail)))
        .collect();
    if let Some(r) = pick(tails) {
        return r;
    }
    let stem = lower
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let close = docs
        .iter()
        .filter(|d| !stem.is_empty() && d.source.to_lowercase().contains(&stem))
        .take(5)
        .collect();
    Resolved::Missing(close)
}

/// Whether `d` is a file under the directory `prefix` (ending in `/`).
pub(super) fn in_dir(d: &DocInfo, prefix: &str) -> bool {
    d.path.as_deref().is_some_and(|p| p.starts_with(prefix))
}

/// What to tell the model when `name` did not resolve to what it needed.
pub(super) fn not_found(name: &str, resolved: Resolved<'_>) -> String {
    let name = attr(name, 200);
    let list = |docs: &[&DocInfo]| {
        docs.iter()
            .map(|d| format!("\"{}\"", attr(&d.key, 200)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match resolved {
        Resolved::Ambiguous(found) => format!(
            "\"{name}\" names {} sources: {}. Use one of these exact names.",
            found.len(),
            list(&found[..found.len().min(8)])
        ),
        Resolved::Missing(close) if !close.is_empty() => format!(
            "there is no source named \"{name}\". Close names: {}.",
            list(&close)
        ),
        Resolved::Dir(prefix) => format!(
            "\"{}\" is a directory. Call context_outline with it to list its documents.",
            attr(&prefix, 200)
        ),
        _ => format!(
            "there is no source named \"{name}\". Call context_outline to see what is available."
        ),
    }
}
