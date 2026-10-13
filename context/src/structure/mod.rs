//! Structure-aware segmentation: documents to passages and sections.
//!
//! Passages follow the document's own structure (Markdown sections and
//! paragraph groups, code items such as functions and impl blocks, one
//! conversation turn) instead of fixed word windows. A passage therefore
//! holds a complete thought: a function with its doc comment, a paragraph
//! with the heading it sits under. Retrieval ranks these units whole and the
//! packer may widen them to their section or neighbours, but never trims
//! them to isolated sentences, so qualifications and exceptions in adjacent
//! sentences stay attached to the claims they qualify.

mod code;
mod code_lex;
mod defs;
mod defs_rules;
mod lang;
mod lines;
mod prose;
mod split;

use serde::{Deserialize, Serialize};

pub(crate) use code::{code_line_flags, is_comment_or_attribute as is_comment_line};
pub(crate) use defs::detect as detect_def;
pub(crate) use lang::Family;
pub use lang::Lang;
pub(crate) use lines::Lines;

use crate::types::{DocKind, PassageKind, SymbolKind};

/// Passage sizing used during segmentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentOptions {
    /// Soft upper bound on a passage, in bytes. Oversized structural units
    /// split at child units, then blank lines, then line boundaries.
    pub max_passage_bytes: usize,
    /// Passages smaller than this merge into a neighbour from the same
    /// section or container.
    pub min_passage_bytes: usize,
}

impl Default for SegmentOptions {
    fn default() -> Self {
        SegmentOptions {
            max_passage_bytes: 1800,
            min_passage_bytes: 240,
        }
    }
}

impl SegmentOptions {
    /// Clamp to sane bounds (`max >= 128`, `min <= max / 2`).
    pub(crate) fn sanitized(self) -> Self {
        let max = self.max_passage_bytes.max(128);
        SegmentOptions {
            max_passage_bytes: max,
            min_passage_bytes: self.min_passage_bytes.min(max / 2),
        }
    }
}

/// Segmentation result for one document.
#[derive(Debug, Default)]
pub(crate) struct Layout {
    pub sections: Vec<SectionSpan>,
    pub passages: Vec<PassageSpan>,
}

/// A heading-delimited section (Markdown only).
#[derive(Debug, Clone)]
pub(crate) struct SectionSpan {
    pub title: String,
    pub depth: u8,
    pub parent: Option<u32>,
    pub start: usize,
    pub end: usize,
}

/// A named definition that a passage introduces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
}

/// One passage: a contiguous byte range of the document.
#[derive(Debug, Clone)]
pub(crate) struct PassageSpan {
    pub start: usize,
    pub end: usize,
    /// 1-based first line.
    pub line_start: u32,
    /// 1-based last line (inclusive).
    pub line_end: u32,
    pub kind: PassageKind,
    pub section: Option<u32>,
    pub symbol: Option<Symbol>,
    /// Enclosing definition for units split out of a larger one (the impl
    /// or class a method belongs to).
    pub container: Option<Symbol>,
    /// Location path shown to the model, e.g. `docs/a.md > Setup > macOS`
    /// or `src/index.rs > impl Index`.
    pub breadcrumb: String,
    /// First line (offset from `line_start`) of the lines that identify the
    /// passage: its heading or definition signature.
    pub anchor_offset: u32,
    /// Number of anchor lines (0 when there is none). The packer keeps the
    /// anchor when it condenses a passage.
    pub anchor_len: u32,
}

impl PassageSpan {
    /// Build a passage covering lines `[l0, l1)`.
    pub(crate) fn from_lines(lines: &Lines<'_>, l0: usize, l1: usize, kind: PassageKind) -> Self {
        PassageSpan {
            start: lines.start(l0),
            end: lines.start(l1),
            line_start: l0 as u32 + 1,
            line_end: l1.max(l0 + 1) as u32,
            kind,
            section: None,
            symbol: None,
            container: None,
            breadcrumb: String::new(),
            anchor_offset: 0,
            anchor_len: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.end - self.start
    }
}

/// Segment `text` into sections and passages.
///
/// `location` is the document's display location (relative path or title),
/// which prefixes every breadcrumb.
pub(crate) fn segment(
    text: &str,
    lang: Lang,
    doc_kind: DocKind,
    location: &str,
    opts: &SegmentOptions,
) -> Layout {
    let opts = opts.sanitized();
    let lines = Lines::new(text);
    if lines.len() == 0 {
        return Layout::default();
    }
    let mut layout = match lang.family() {
        Family::Markdown => prose::markdown(&lines, location, &opts),
        Family::Plain => prose::plain(&lines, location, &opts),
        Family::Brace | Family::Indent => code::segment(&lines, lang, location, &opts),
    };
    split::normalize(&mut layout, &lines, text, &opts);
    if doc_kind == DocKind::Turn {
        for p in &mut layout.passages {
            p.kind = PassageKind::Turn;
        }
    }
    layout
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
