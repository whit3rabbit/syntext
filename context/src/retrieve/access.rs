//! Uniform access to a candidate's document, whether it is stored in a
//! segment or was read live from a syntext index because the store was
//! stale or did not hold the file.

use std::sync::Arc;

use crate::error::{ContextError, Result};
use crate::store::StoreSnapshot;
use crate::structure::Layout;
use crate::types::Trust;

/// A document segmented on the fly from live file content.
pub(crate) struct LiveDoc {
    pub key: String,
    pub path: String,
    pub text: Arc<str>,
    pub layout: Layout,
    pub scope: String,
    pub trust: Trust,
    pub source_hash: String,
}

/// Structure of one passage, independent of where the document lives.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PassageInfo {
    pub start: usize,
    pub end: usize,
    pub line_start: u32,
    pub line_end: u32,
    /// Document-local section index.
    pub section: Option<u32>,
}

/// Text and structure of a candidate's document.
#[derive(Clone)]
pub(crate) enum DocAccess {
    Stored {
        snap: Arc<StoreSnapshot>,
        seg: u32,
        doc: u32,
    },
    Live(Arc<LiveDoc>),
}

impl DocAccess {
    /// Passages of the document, in order.
    pub(crate) fn passages(&self) -> Vec<PassageInfo> {
        match self {
            DocAccess::Stored { snap, seg, doc } => {
                let r = snap.reader(*seg);
                let d = &r.docs[*doc as usize];
                let first = d.first_passage;
                r.passages[first as usize..(first + d.passage_count) as usize]
                    .iter()
                    .map(|p| PassageInfo {
                        start: p.start as usize,
                        end: p.end as usize,
                        line_start: p.line_start,
                        line_end: p.line_end,
                        section: p.section.map(|s| s - d.first_section),
                    })
                    .collect()
            }
            DocAccess::Live(live) => live
                .layout
                .passages
                .iter()
                .map(|p| PassageInfo {
                    start: p.start,
                    end: p.end,
                    line_start: p.line_start,
                    line_end: p.line_end,
                    section: p.section,
                })
                .collect(),
        }
    }

    /// Byte range of section `section` (document-local index).
    pub(crate) fn section(&self, section: u32) -> Option<(usize, usize)> {
        match self {
            DocAccess::Stored { snap, seg, doc } => {
                let r = snap.reader(*seg);
                let d = &r.docs[*doc as usize];
                if section >= d.section_count {
                    return None;
                }
                let s = r.sections.get((d.first_section + section) as usize)?;
                Some((s.start as usize, s.end as usize))
            }
            DocAccess::Live(live) => {
                let s = live.layout.sections.get(section as usize)?;
                Some((s.start, s.end))
            }
        }
    }

    /// Text bytes `[start, end)` of the document.
    pub(crate) fn read(&self, start: usize, end: usize) -> Result<String> {
        match self {
            DocAccess::Stored { snap, seg, doc } => snap.reader(*seg).text_range(*doc, start, end),
            DocAccess::Live(live) => live
                .text
                .get(start..end)
                .map(str::to_string)
                .ok_or_else(|| ContextError::corrupt("live text range out of bounds")),
        }
    }

    /// Same-document identity, for merging candidates.
    pub(crate) fn same_doc(&self, other: &DocAccess) -> bool {
        match (self, other) {
            (
                DocAccess::Stored { seg, doc, .. },
                DocAccess::Stored {
                    seg: s2, doc: d2, ..
                },
            ) => seg == s2 && doc == d2,
            (DocAccess::Live(a), DocAccess::Live(b)) => Arc::ptr_eq(a, b) || a.key == b.key,
            _ => false,
        }
    }
}
