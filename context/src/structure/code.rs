//! Code segmentation: top-level items, recursively split when oversized.
//!
//! Every line gets a nesting depth (brace depth from a small lexer that skips
//! strings and comments, or indentation width). A *unit* is a run of lines at
//! one nesting level that ends at a blank line once nesting has returned to
//! that level, or right after a nested block closes. Doc comments and
//! attributes touching a definition therefore stay with it. A unit larger
//! than the passage budget is split into its child units (methods of an impl
//! block, statement groups of a long function), with the parent's header
//! attached to the first child and the parent named in each child's
//! breadcrumb.

use super::lang::{Family, Lang};
use super::lines::Lines;
use super::{defs, Layout, PassageSpan, SegmentOptions, Symbol};
use crate::types::{PassageKind, SymbolKind};

#[path = "code_outline.rs"]
mod outline;
pub(super) use outline::definitions;

/// How many nested container levels are split before falling back to
/// line-based splitting.
const MAX_SPLIT_DEPTH: usize = 4;
/// How many leading code lines are searched for a unit's definition.
const DEF_SCAN_LINES: usize = 8;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LineInfo {
    pub depth_start: u32,
    pub depth_end: u32,
    /// Deepest nesting reached anywhere on the line.
    pub max_depth: u32,
    pub blank: bool,
    /// Block terminator in an indentation language (`end`, `fi`, `}`).
    pub closer: bool,
    /// The line starts inside a string literal or block comment, so it
    /// cannot introduce a definition.
    pub in_literal: bool,
}

pub(super) fn segment(
    lines: &Lines<'_>,
    lang: Lang,
    location: &str,
    opts: &SegmentOptions,
) -> Layout {
    let info = match lang.family() {
        Family::Brace => super::code_lex::brace_infos(lines, lang),
        _ => indent_infos(lines, lang),
    };
    let mut seg = Segmenter {
        lines,
        lang,
        info: &info,
        opts,
        out: Vec::new(),
    };
    for (s, e) in seg.units(0, lines.len(), 0) {
        seg.emit(s, e, 0, location, None, None);
    }
    Layout {
        sections: Vec::new(),
        passages: seg.out,
    }
}

struct Segmenter<'a, 'b> {
    lines: &'a Lines<'b>,
    lang: Lang,
    info: &'a [LineInfo],
    opts: &'a SegmentOptions,
    out: Vec<PassageSpan>,
}

impl Segmenter<'_, '_> {
    /// Split lines `[l0, l1)` into units at nesting level `base`.
    fn units(&self, l0: usize, l1: usize, base: u32) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut cur: Option<usize> = None;
        let mut last: Option<usize> = None;
        for i in l0..l1 {
            let li = self.info[i];
            if li.blank {
                let settled = last.is_none_or(|p| self.info[p].depth_end <= base);
                if let Some(s) = cur {
                    if li.depth_start <= base && settled {
                        out.push((s, i));
                        cur = None;
                    }
                }
                continue;
            }
            if li.depth_start <= base {
                let line = self.lines.line(i);
                let closed = last.is_some_and(|p| {
                    let pi = self.info[p];
                    pi.max_depth > base || pi.closer
                });
                if let Some(s) = cur {
                    let forced = self.lang == Lang::Toml && line.starts_with('[');
                    if (closed || forced) && !is_continuation(line, self.lang) {
                        out.push((s, i));
                        cur = Some(i);
                    }
                } else {
                    cur = Some(i);
                }
            } else if cur.is_none() {
                cur = Some(i);
            }
            last = Some(i);
        }
        if let Some(s) = cur {
            out.push((s, l1));
        }
        out
    }

    /// Turn unit `[s, e)` into passages, splitting it when oversized.
    /// `head` is the definition a split function's first piece inherits:
    /// that piece holds the signature, so it is where the function is
    /// defined.
    fn emit(
        &mut self,
        s: usize,
        e: usize,
        depth: usize,
        crumb: &str,
        parent: Option<&Symbol>,
        head: Option<(Symbol, usize)>,
    ) {
        let Some((s, e)) = self.lines.trim_blank(s, e) else {
            return;
        };
        let (found, found_line) = self.find_def(s, e, parent);
        let inherited = found.is_none() && head.is_some();
        let (symbol, def_line) = match (found, head) {
            (Some(sym), _) => (Some(sym), found_line),
            (None, Some((sym, line))) if (s..e).contains(&line) => (Some(sym), Some(line)),
            _ => (None, None),
        };
        if self.lines.bytes(s, e) > self.opts.max_passage_bytes && depth < MAX_SPLIT_DEPTH {
            if let Some(children) = self.children(s, e) {
                let child_crumb = match &symbol {
                    Some(sym) if !inherited => {
                        format!("{crumb} > {} {}", sym.kind.as_str(), sym.name)
                    }
                    _ => crumb.to_string(),
                };
                let container = if inherited {
                    parent.cloned()
                } else {
                    symbol.clone().or_else(|| parent.cloned())
                };
                let callable = symbol.clone().filter(|sym| !sym.kind.is_container());
                for (i, (cs, ce)) in children.into_iter().enumerate() {
                    let head = callable.clone().zip(def_line).filter(|_| i == 0);
                    self.emit(cs, ce, depth + 1, &child_crumb, container.as_ref(), head);
                }
                return;
            }
        }
        let kind = if self.lang.is_data() {
            PassageKind::Data
        } else if symbol.is_some() {
            PassageKind::Definition
        } else {
            PassageKind::Code
        };
        let mut p = PassageSpan::from_lines(self.lines, s, e, kind);
        if let Some(d) = def_line {
            p.anchor_offset = (d - s) as u32;
            p.anchor_len = self.signature_len(d, e);
        }
        p.symbol = symbol;
        p.container = parent.cloned();
        p.breadcrumb = crumb.to_string();
        self.out.push(p);
    }

    /// Split a unit into child units, attaching its header to the first
    /// child and its trailing lines to the last. `None` when the unit has no
    /// inner structure to split on.
    fn children(&self, s: usize, e: usize) -> Option<Vec<(usize, usize)>> {
        let body: Vec<usize> = (s..e)
            .filter(|&i| !self.info[i].blank && self.info[i].depth_start > self.info[s].depth_start)
            .collect();
        let (&first, &last) = (body.first()?, body.last()?);
        let child_base = match self.lang.family() {
            Family::Brace => self.info[s].depth_start + 1,
            _ => body.iter().map(|&i| self.info[i].depth_start).min()?,
        };
        let mut kids = self.units(first, last + 1, child_base);
        if kids.len() < 2 {
            return None;
        }
        kids[0].0 = s;
        let n = kids.len();
        kids[n - 1].1 = e;
        for k in 0..n - 1 {
            kids[k].1 = kids[k + 1].0;
        }
        Some(kids)
    }

    /// Primary definition of unit `[s, e)` and the line it is on.
    fn find_def(
        &self,
        s: usize,
        e: usize,
        parent: Option<&Symbol>,
    ) -> (Option<Symbol>, Option<usize>) {
        let mut scanned = 0;
        for i in s..e {
            if scanned >= DEF_SCAN_LINES {
                break;
            }
            let line = self.lines.line(i);
            let t = line.trim_start();
            let li = self.info[i];
            if li.blank || li.in_literal || is_comment_or_attribute(t, self.lang) {
                continue;
            }
            scanned += 1;
            if let Some(def) = defs::detect(line, self.lang) {
                // The first child of a split unit starts with the parent's
                // own header (`impl Index {`); that names the parent.
                if parent.is_some_and(|p| p.name == def.name && p.kind == def.kind) {
                    continue;
                }
                let in_type = parent.is_some_and(|p| {
                    matches!(
                        p.kind,
                        SymbolKind::Impl
                            | SymbolKind::Class
                            | SymbolKind::Struct
                            | SymbolKind::Interface
                            | SymbolKind::Enum
                    )
                });
                let kind = if in_type && def.kind == SymbolKind::Function {
                    SymbolKind::Method
                } else {
                    def.kind
                };
                return (
                    Some(Symbol {
                        name: def.name,
                        kind,
                    }),
                    Some(i),
                );
            }
        }
        (None, None)
    }

    /// Number of lines in the signature starting at `def_line`: through
    /// the first line that opens a block or ends the declaration, at most 4.
    fn signature_len(&self, def_line: usize, e: usize) -> u32 {
        let mut end = def_line;
        while end + 1 < e && end < def_line + 3 {
            let t = self.lines.line(end).trim_end();
            let opens = t.ends_with(['{', ':', ';', '}', '=']);
            if opens || self.info[end].depth_end > self.info[def_line].depth_start {
                break;
            }
            end += 1;
        }
        (end - def_line + 1) as u32
    }
}

/// Lines that belong to the preceding construct even at the base level.
fn is_continuation(line: &str, lang: Lang) -> bool {
    let t = line.trim_start();
    let word = t
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("");
    if t.starts_with([')', ']', '}', '.']) {
        return true;
    }
    match lang {
        Lang::Python => matches!(word, "elif" | "else" | "except" | "finally"),
        Lang::Ruby => matches!(
            word,
            "end" | "else" | "elsif" | "rescue" | "ensure" | "when"
        ),
        Lang::Lua => matches!(word, "end" | "else" | "elseif" | "until"),
        Lang::Shell => matches!(
            word,
            "fi" | "done" | "esac" | "else" | "elif" | "then" | "do"
        ),
        Lang::Yaml => t.starts_with("- ") || t == "-",
        _ => matches!(word, "else" | "catch" | "finally" | "except"),
    }
}

/// Block terminators in indentation-family languages.
fn is_closer(line: &str, lang: Lang) -> bool {
    let t = line.trim();
    match lang {
        Lang::Ruby | Lang::Lua => t == "end" || t.starts_with("end ") || t.starts_with("end."),
        Lang::Shell => matches!(t, "fi" | "done" | "esac" | "}" | ";;"),
        _ => matches!(t, ")" | "]" | "}" | "})" | "];" | ");" | "};"),
    }
}

/// True for comment, attribute, and decorator lines (`t` is left-trimmed).
pub(crate) fn is_comment_or_attribute(t: &str, lang: Lang) -> bool {
    t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with("#[")
        || t.starts_with("#!")
        || (t.starts_with('#') && lang.line_comments().contains(&"#"))
        || (t.starts_with("--") && lang.line_comments().contains(&"--"))
        || (t.starts_with('@') && lang != Lang::Php)
        || t.starts_with("\"\"\"")
}

/// For each line of `text`, whether it starts outside string literals and
/// block comments (always true for indentation languages, which are not
/// lexed).
pub(crate) fn code_line_flags(text: &str, lang: Lang) -> Vec<bool> {
    let lines = Lines::new(text);
    match lang.family() {
        Family::Brace => super::code_lex::brace_infos(&lines, lang)
            .iter()
            .map(|i| !i.in_literal)
            .collect(),
        _ => vec![true; lines.len()],
    }
}

/// Indentation-based depths. A blank line takes the depth of the next
/// non-blank line, so a blank line inside an indented body never ends the
/// enclosing top-level unit.
fn indent_infos(lines: &Lines<'_>, lang: Lang) -> Vec<LineInfo> {
    let n = lines.len();
    let mut out = vec![LineInfo::default(); n];
    let mut next_depth = 0u32;
    for i in (0..n).rev() {
        let line = lines.line(i);
        if line.trim().is_empty() {
            out[i] = LineInfo {
                depth_start: next_depth,
                depth_end: next_depth,
                max_depth: next_depth,
                blank: true,
                closer: false,
                in_literal: false,
            };
            continue;
        }
        let width = indent_width(line);
        next_depth = width;
        out[i] = LineInfo {
            depth_start: width,
            depth_end: width,
            max_depth: width,
            blank: false,
            closer: width == 0 && is_closer(line, lang),
            in_literal: false,
        };
    }
    out
}

fn indent_width(line: &str) -> u32 {
    let mut w = 0u32;
    for c in line.chars() {
        match c {
            ' ' => w += 1,
            '\t' => w += 4,
            _ => break,
        }
    }
    w
}

#[cfg(test)]
#[path = "code_tests.rs"]
mod tests;
