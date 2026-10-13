//! `context_outline`: what the corpus, a directory, or one document holds,
//! at the most detail the result budget allows.
//!
//! A listing shrinks in steps until it fits: documents with their top-level
//! sections, then documents alone, then directories with counts, then
//! top-level directories cut off with a count of the rest. A document
//! outline drops its deepest levels first. Every name comes from the
//! documents themselves, so the body is sanitized like evidence.

use std::collections::BTreeMap;

use super::args::Args;
use super::source::{in_dir, normalize, not_found, resolve, visible, DocInfo, Resolved};
use super::{ToolOptions, ToolResult, OUTLINE_TOOL};
use crate::engine::ContextEngine;
use crate::error::Result;
use crate::render::sanitize::{attr, sanitize};
use crate::store::StoreSnapshot;
use crate::structure::{outline, OutlineItem};
use crate::types::{DocKind, Trust};

/// Element wrapping an outline.
const TAG: &str = "outline";
/// Top-level entries named per document in a listing.
const ENTRIES_PER_DOC: usize = 6;
/// Nesting kept from a breadcrumb (segments are untrusted input).
const MAX_DEPTH: usize = 12;

pub(super) fn run(engine: &ContextEngine, args: &Args, opts: &ToolOptions) -> Result<ToolResult> {
    let name = match args.text("source") {
        Ok(name) => name
            .map(|n| normalize(&n))
            .filter(|n| !n.is_empty() && n != "."),
        Err(e) => return Ok(engine.tool_error(OUTLINE_TOOL, &e, opts)),
    };
    let snap = engine.store().snapshot();
    let docs = visible(&snap, &opts.scopes(engine), false);
    let Some(name) = name else {
        let all: Vec<&DocInfo> = docs.iter().collect();
        return listing(engine, &snap, &all, "", opts);
    };
    match resolve(&docs, &name, true) {
        Resolved::Doc(d) => document(engine, &snap, d, opts),
        Resolved::Dir(prefix) => {
            let inside: Vec<&DocInfo> = docs.iter().filter(|d| in_dir(d, &prefix)).collect();
            listing(engine, &snap, &inside, &prefix, opts)
        }
        other => Ok(engine.tool_error(OUTLINE_TOOL, &not_found(&name, other), opts)),
    }
}

/// Sections or definitions of `doc`, from its stored text.
fn items(snap: &StoreSnapshot, doc: &DocInfo) -> Result<Vec<OutlineItem>> {
    let r = snap.reader(doc.loc.seg);
    let lang = r.docs[doc.loc.doc as usize].lang;
    let text = r.doc_text(doc.loc.doc)?;
    let mut items = outline(&text, lang);
    for item in &mut items {
        item.depth = item.depth.min(MAX_DEPTH);
        item.name = one_line(&item.name);
    }
    Ok(items)
}

/// Items nested less than `max` deep, one per line.
fn render_items(items: &[OutlineItem], max: usize) -> Vec<String> {
    items
        .iter()
        .filter(|i| i.depth < max)
        .map(|i| {
            let pad = "  ".repeat(i.depth);
            match i.lines {
                (a, b) if a == b => format!("{pad}{} (line {a})", i.name),
                (a, b) => format!("{pad}{} (lines {a}-{b})", i.name),
            }
        })
        .collect()
}

/// Names of a document's top-level items; a lone title gives way to the
/// sections under it.
fn top_entries(items: &[OutlineItem]) -> Vec<&str> {
    let level = match items.iter().filter(|i| i.depth == 0).count() {
        1 if items.iter().any(|i| i.depth == 1) => 1,
        _ => 0,
    };
    items
        .iter()
        .filter(|i| i.depth == level)
        .map(|i| i.name.as_str())
        .collect()
}

fn document(
    engine: &ContextEngine,
    snap: &StoreSnapshot,
    doc: &DocInfo,
    opts: &ToolOptions,
) -> Result<ToolResult> {
    let head = format!(
        "<{TAG} source=\"{}\" lines=\"1-{}\" trust=\"{}\">",
        attr(&doc.source, 200),
        doc.lines,
        doc.trust.as_str()
    );
    let fit = Fit::new(engine, head, opts.result_tokens);
    let items = items(snap, doc)?;
    let trust = Some(doc.trust);
    if items.is_empty() {
        let body = vec![format!(
            "No sections or definitions. Read its {} lines with context_read.",
            doc.lines
        )];
        return Ok(finish(engine, fit.text(&body, None), trust, opts));
    }
    let deepest = items.iter().map(|i| i.depth + 1).max().unwrap_or(1);
    let mut lines = Vec::new();
    for max in (1..=deepest).rev() {
        lines = render_items(&items, max);
        let note = (max < deepest).then_some("Entries nested deeper are left out.");
        let text = fit.text(&lines, note);
        if fit.fits(&text) {
            return Ok(finish(engine, text, trust, opts));
        }
    }
    Ok(finish(engine, fit.prefix(&lines, "entries"), trust, opts))
}

/// Where a document is listed: its directory under `prefix`, or a group
/// for documents without a path. Sorts top level first and groups last.
fn group_of(d: &DocInfo, prefix: &str) -> (u8, String) {
    match (&d.path, d.kind) {
        (Some(p), _) => {
            let rel = p.strip_prefix(prefix).unwrap_or(p);
            let dir = rel.rfind('/').map_or("", |i| &rel[..=i]);
            (u8::from(!dir.is_empty()), dir.to_string())
        }
        (None, DocKind::External) => (3, "[external]".to_string()),
        (None, _) => (2, "[notes]".to_string()),
    }
}

/// How a document is named inside its group.
fn short_name(d: &DocInfo, prefix: &str, dir: &str) -> String {
    let full = d.path.as_deref().unwrap_or(&d.source);
    let rel = full.strip_prefix(prefix).unwrap_or(full);
    one_line(rel.strip_prefix(dir).unwrap_or(rel))
}

fn listing(
    engine: &ContextEngine,
    snap: &StoreSnapshot,
    docs: &[&DocInfo],
    prefix: &str,
    opts: &ToolOptions,
) -> Result<ToolResult> {
    let trust = docs.iter().map(|d| d.trust).min();
    let what = if prefix.is_empty() {
        "all sources"
    } else {
        prefix
    };
    let head = format!(
        "<{TAG} source=\"{}\" documents=\"{}\">",
        attr(what, 200),
        docs.len()
    );
    let fit = Fit::new(engine, head, opts.result_tokens);
    if docs.is_empty() {
        let body = vec!["No documents.".to_string()];
        return Ok(finish(engine, fit.text(&body, None), trust, opts));
    }
    let mut groups: BTreeMap<(u8, String), Vec<&DocInfo>> = BTreeMap::new();
    for d in docs {
        groups.entry(group_of(d, prefix)).or_default().push(d);
    }
    // Documents, with their top-level entries and then without, when one
    // line each could plausibly fit.
    if docs.len() * 6 <= opts.result_tokens {
        for detail in [true, false] {
            let mut lines = Vec::new();
            for ((_, dir), members) in &groups {
                let pad = if dir.is_empty() { "" } else { "  " };
                if !dir.is_empty() {
                    lines.push(one_line(dir));
                }
                for d in members {
                    let mut line =
                        format!("{pad}{} ({} lines)", short_name(d, prefix, dir), d.lines);
                    if detail {
                        let items = items(snap, d)?;
                        let entries = top_entries(&items);
                        if !entries.is_empty() {
                            let shown = &entries[..entries.len().min(ENTRIES_PER_DOC)];
                            line.push_str(": ");
                            line.push_str(&shown.join("; "));
                            if entries.len() > shown.len() {
                                line.push_str("; ...");
                            }
                        }
                    }
                    lines.push(line);
                }
            }
            let note = "Call context_outline with a file for its sections, or context_read \
                        to read it.";
            let text = fit.text(&lines, Some(note));
            if fit.fits(&text) {
                return Ok(finish(engine, text, trust, opts));
            }
        }
    }
    let note = "Call context_outline with a directory to list its documents.";
    let dirs: Vec<String> = groups
        .iter()
        .map(|((_, dir), members)| {
            let name = if dir.is_empty() { "(top level)" } else { dir };
            format!("{} ({} documents)", one_line(name), members.len())
        })
        .collect();
    let text = fit.text(&dirs, Some(note));
    if fit.fits(&text) {
        return Ok(finish(engine, text, trust, opts));
    }
    // Top-level directories only, cut off to fit.
    let mut top: BTreeMap<(u8, String), usize> = BTreeMap::new();
    for ((rank, dir), members) in &groups {
        let first = match dir.find('/') {
            Some(i) if *rank == 1 => &dir[..=i],
            _ => dir.as_str(),
        };
        *top.entry((*rank, first.to_string())).or_default() += members.len();
    }
    let lines: Vec<String> = top
        .iter()
        .map(|((_, dir), n)| {
            let name = if dir.is_empty() { "(top level)" } else { dir };
            format!("{} ({n} documents)", one_line(name))
        })
        .collect();
    let text = fit.text(&lines, Some(note));
    if fit.fits(&text) {
        return Ok(finish(engine, text, trust, opts));
    }
    Ok(finish(
        engine,
        fit.prefix(&lines, "directories"),
        trust,
        opts,
    ))
}

/// Renders an outline and checks it against the budget.
struct Fit<'a> {
    engine: &'a ContextEngine,
    head: String,
    budget: usize,
}

impl<'a> Fit<'a> {
    fn new(engine: &'a ContextEngine, head: String, budget: usize) -> Self {
        Fit {
            engine,
            head,
            budget,
        }
    }

    /// The outline element holding `lines`, then `note` outside it.
    fn text(&self, lines: &[String], note: Option<&str>) -> String {
        let render = self.engine.render_options();
        let body = lines.join("\n");
        let (body, _) = sanitize(&body, TAG, &render.sanitize);
        let (body, _) = sanitize(&body, &render.tag, &render.sanitize);
        let mut out = format!("{}\n{body}\n</{TAG}>", self.head);
        if let Some(note) = note {
            out.push_str("\n\n");
            out.push_str(note);
        }
        out
    }

    fn fits(&self, text: &str) -> bool {
        self.engine.counter().count(text) <= self.budget
    }

    /// As many leading `lines` as fit, with a count of the rest.
    fn prefix(&self, lines: &[String], what: &str) -> String {
        let with = |k: usize| {
            let mut kept = lines[..k].to_vec();
            if k < lines.len() {
                kept.push(format!("[... {} more {what} ...]", lines.len() - k));
            }
            self.text(&kept, None)
        };
        let (mut lo, mut hi) = (0, lines.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.fits(&with(mid)) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        with(lo)
    }
}

fn finish(
    engine: &ContextEngine,
    content: String,
    trust: Option<Trust>,
    opts: &ToolOptions,
) -> ToolResult {
    engine.tool_text(OUTLINE_TOOL, content, trust, opts)
}

/// A name on one line.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
}
