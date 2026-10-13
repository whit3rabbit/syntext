//! Prose segmentation: Markdown sections and paragraph groups, plain text
//! paragraphs.
//!
//! Passages are groups of whole blocks (paragraphs, lists, tables, fenced
//! code) inside one section, so a heading always travels with the text under
//! it and no paragraph is cut unless it alone exceeds the size budget.

use super::lines::Lines;
use super::{Layout, PassageSpan, SectionSpan, SegmentOptions};
use crate::types::PassageKind;

#[derive(Debug)]
enum Block {
    Heading {
        level: u8,
        title: String,
        l0: usize,
        l1: usize,
    },
    Content {
        l0: usize,
        l1: usize,
        code: bool,
    },
}

pub(super) fn markdown(lines: &Lines<'_>, location: &str, opts: &SegmentOptions) -> Layout {
    let mut b = Builder::new(lines, location, opts);
    for block in scan_markdown(lines) {
        match block {
            Block::Heading {
                level,
                title,
                l0,
                l1,
            } => b.heading(level, title, l0, l1),
            Block::Content { l0, l1, code } => b.content(l0, l1, code),
        }
    }
    b.finish()
}

pub(super) fn plain(lines: &Lines<'_>, location: &str, opts: &SegmentOptions) -> Layout {
    let mut b = Builder::new(lines, location, opts);
    let mut i = 0;
    while i < lines.len() {
        if lines.is_blank(i) {
            i += 1;
            continue;
        }
        let s = i;
        while i < lines.len() && !lines.is_blank(i) {
            i += 1;
        }
        b.content(s, i, false);
    }
    b.finish()
}

/// Passage under construction.
struct Pending {
    l0: usize,
    l1: usize,
    section: Option<u32>,
    has_content: bool,
    heading_lines: u32,
    code_lines: usize,
}

struct Builder<'a, 'b> {
    lines: &'a Lines<'b>,
    location: &'a str,
    opts: &'a SegmentOptions,
    sections: Vec<SectionSpan>,
    stack: Vec<(u8, u32)>,
    passages: Vec<PassageSpan>,
    pending: Option<Pending>,
}

impl<'a, 'b> Builder<'a, 'b> {
    fn new(lines: &'a Lines<'b>, location: &'a str, opts: &'a SegmentOptions) -> Self {
        Builder {
            lines,
            location,
            opts,
            sections: Vec::new(),
            stack: Vec::new(),
            passages: Vec::new(),
            pending: None,
        }
    }

    fn heading(&mut self, level: u8, title: String, l0: usize, l1: usize) {
        self.flush();
        let at = self.lines.start(l0);
        while let Some(&(lv, idx)) = self.stack.last() {
            if lv < level {
                break;
            }
            self.sections[idx as usize].end = at;
            self.stack.pop();
        }
        let parent = self.stack.last().map(|&(_, idx)| idx);
        let idx = self.sections.len() as u32;
        self.sections.push(SectionSpan {
            title,
            depth: level,
            parent,
            start: at,
            end: at,
        });
        self.stack.push((level, idx));
        self.pending = Some(Pending {
            l0,
            l1,
            section: Some(idx),
            has_content: false,
            heading_lines: (l1 - l0) as u32,
            code_lines: 0,
        });
    }

    fn content(&mut self, l0: usize, l1: usize, code: bool) {
        let section = self.stack.last().map(|&(_, idx)| idx);
        let code_lines = if code { l1 - l0 } else { 0 };
        if let Some(p) = &mut self.pending {
            let fits = self.lines.bytes(p.l0, l1) <= self.opts.max_passage_bytes;
            // A heading always takes the first block after it, even when the
            // pair is oversized; the splitter keeps the heading in front.
            if p.section == section && (fits || !p.has_content) {
                p.l1 = l1;
                p.has_content = true;
                p.code_lines += code_lines;
                return;
            }
        }
        self.flush();
        self.pending = Some(Pending {
            l0,
            l1,
            section,
            has_content: true,
            heading_lines: 0,
            code_lines,
        });
    }

    fn flush(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        if !p.has_content {
            // A heading with nothing under it lives on in the section table
            // and in its children's breadcrumbs.
            return;
        }
        let total = p.l1 - p.l0;
        let kind = if p.code_lines * 10 > total * 6 {
            PassageKind::Code
        } else {
            PassageKind::Prose
        };
        let mut span = PassageSpan::from_lines(self.lines, p.l0, p.l1, kind);
        span.section = p.section;
        span.anchor_len = p.heading_lines;
        span.breadcrumb = self.breadcrumb(p.section);
        self.passages.push(span);
    }

    fn breadcrumb(&self, section: Option<u32>) -> String {
        let mut chain = Vec::new();
        let mut cur = section;
        while let Some(idx) = cur {
            let s = &self.sections[idx as usize];
            chain.push(s.title.as_str());
            cur = s.parent;
        }
        let mut out = self.location.to_string();
        for title in chain.iter().rev() {
            out.push_str(" > ");
            out.push_str(title);
        }
        out
    }

    fn finish(mut self) -> Layout {
        self.flush();
        let end = self.lines.start(self.lines.len());
        for (_, idx) in self.stack.drain(..) {
            self.sections[idx as usize].end = end;
        }
        Layout {
            sections: self.sections,
            passages: self.passages,
        }
    }
}

fn scan_markdown(lines: &Lines<'_>) -> Vec<Block> {
    let n = lines.len();
    let mut out = Vec::new();
    let mut i = 0;
    if n > 1 && lines.line(0).trim_end() == "---" {
        let close = (1..n.min(200)).find(|&j| matches!(lines.line(j).trim_end(), "---" | "..."));
        if let Some(j) = close {
            out.push(Block::Content {
                l0: 0,
                l1: j + 1,
                code: true,
            });
            i = j + 1;
        }
    }
    while i < n {
        let line = lines.line(i);
        if lines.is_blank(i) {
            i += 1;
            continue;
        }
        if let Some((ch, len)) = fence_open(line) {
            let close = (i + 1..n).find(|&j| fence_closes(lines.line(j), ch, len));
            let end = close.map_or(n, |j| j + 1);
            out.push(Block::Content {
                l0: i,
                l1: end,
                code: true,
            });
            i = end;
            continue;
        }
        if let Some((level, title)) = atx_heading(line) {
            out.push(Block::Heading {
                level,
                title,
                l0: i,
                l1: i + 1,
            });
            i += 1;
            continue;
        }
        if i + 1 < n && !is_block_marker(line) {
            if let Some(level) = setext_level(lines.line(i + 1)) {
                let title = line.trim().to_string();
                out.push(Block::Heading {
                    level,
                    title,
                    l0: i,
                    l1: i + 2,
                });
                i += 2;
                continue;
            }
        }
        let s = i;
        i += 1;
        while i < n
            && !lines.is_blank(i)
            && fence_open(lines.line(i)).is_none()
            && atx_heading(lines.line(i)).is_none()
        {
            i += 1;
        }
        out.push(Block::Content {
            l0: s,
            l1: i,
            code: false,
        });
    }
    out
}

/// `#`-style heading: up to three spaces, 1-6 `#`, then space or end.
fn atx_heading(line: &str) -> Option<(u8, String)> {
    let t = strip_indent(line)?;
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &t[hashes..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim_end().to_string();
    Some((hashes as u8, title))
}

fn setext_level(line: &str) -> Option<u8> {
    let t = line.trim();
    if !t.is_empty() && t.bytes().all(|b| b == b'=') {
        Some(1)
    } else if t.len() >= 2 && t.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

/// Lines that start a list item, table row, or quote, which never become
/// setext headings.
fn is_block_marker(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    if t.starts_with('|') || t.starts_with('>') {
        return true;
    }
    let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

fn fence_open(line: &str) -> Option<(u8, usize)> {
    let t = strip_indent(line)?;
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = t.bytes().take_while(|&b| b == ch).count();
    (len >= 3).then_some((ch, len))
}

fn fence_closes(line: &str, ch: u8, len: usize) -> bool {
    let Some(t) = strip_indent(line) else {
        return false;
    };
    let run = t.bytes().take_while(|&b| b == ch).count();
    run >= len && t[run..].trim().is_empty()
}

/// Strip up to three leading spaces; `None` when the line is indented code.
fn strip_indent(line: &str) -> Option<&str> {
    let spaces = line.bytes().take_while(|&b| b == b' ').count();
    (spaces <= 3).then(|| &line[spaces..])
}

#[cfg(test)]
#[path = "prose_tests.rs"]
mod tests;
