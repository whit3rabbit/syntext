//! Every named definition of a source file with its nesting, for outlines.
//!
//! Segmentation splits a unit into its children only when it is oversized,
//! so a small impl block is one passage and its methods are not passages of
//! their own. An outline wants each of them: this walks the same units and
//! definition rules, but descends into every container (class, impl,
//! module, ...) whatever its size. Function bodies are not descended into;
//! their inner units are statements, not definitions.

use super::{defs, indent_infos, is_comment_or_attribute, Segmenter, MAX_SPLIT_DEPTH};
use crate::structure::lang::{Family, Lang};
use crate::structure::lines::Lines;
use crate::structure::{OutlineItem, SegmentOptions, Symbol};
use crate::types::SymbolKind;

/// Definitions of `lines`, in document order.
pub(crate) fn definitions(lines: &Lines<'_>, lang: Lang) -> Vec<OutlineItem> {
    let info = match lang.family() {
        Family::Brace => crate::structure::code_lex::brace_infos(lines, lang),
        Family::Indent => indent_infos(lines, lang),
        _ => return Vec::new(),
    };
    let opts = SegmentOptions::default();
    let seg = Segmenter {
        lines,
        lang,
        info: &info,
        opts: &opts,
        out: Vec::new(),
    };
    let mut out = Vec::new();
    for unit in seg.units(0, lines.len(), 0) {
        walk(&seg, unit, false, 0, None, &mut out);
    }
    out
}

/// Record the definition of unit `[s, e)` and, for a container, its
/// members. `first` marks a container's first child, which starts with the
/// container's own header.
fn walk(
    seg: &Segmenter<'_, '_>,
    (s, e): (usize, usize),
    first: bool,
    depth: usize,
    parent: Option<&Symbol>,
    out: &mut Vec<OutlineItem>,
) {
    let Some((s, e)) = seg.lines.trim_blank(s, e) else {
        return;
    };
    let (Some(symbol), Some(line)) = seg.find_def(s, e, parent) else {
        return;
    };
    let mut start = s;
    if first {
        // From the definition back over its own comments and attributes,
        // stopping at the container's header.
        start = line;
        while start > s && is_comment_or_attribute(seg.lines.line(start - 1).trim_start(), seg.lang)
        {
            start -= 1;
        }
    }
    let at = out.len();
    out.push(OutlineItem {
        name: label(seg, line, &symbol),
        depth,
        lines: (start as u32 + 1, e as u32),
    });
    // One-line definitions that share the unit (`mod a;` after `mod b;`).
    siblings(seg, line, e, depth, parent, at, out);
    if !symbol.kind.is_container() || depth + 1 >= MAX_SPLIT_DEPTH {
        return;
    }
    for (unit, first) in members(seg, s, e) {
        walk(seg, unit, first, depth + 1, Some(&symbol), out);
    }
}

/// Member units of container `[s, e)`, flagged when one starts with the
/// container's header. Unlike `children`, a lone member counts.
fn members(seg: &Segmenter<'_, '_>, s: usize, e: usize) -> Vec<((usize, usize), bool)> {
    if let Some(kids) = seg.children(s, e) {
        return kids
            .into_iter()
            .enumerate()
            .map(|(i, k)| (k, i == 0))
            .collect();
    }
    let top = seg.info[s].depth_start;
    let body: Vec<usize> = (s..e)
        .filter(|&i| !seg.info[i].blank && seg.info[i].depth_start > top)
        .collect();
    let (Some(&first), Some(&last)) = (body.first(), body.last()) else {
        return Vec::new();
    };
    let base = match seg.lang.family() {
        Family::Brace => top + 1,
        _ => body
            .iter()
            .map(|&i| seg.info[i].depth_start)
            .min()
            .unwrap_or(top + 1),
    };
    match seg.units(first, last + 1, base).as_slice() {
        [only] => vec![(*only, false)],
        _ => Vec::new(),
    }
}

/// `<kind> <name>`, or `impl <Trait> for <Type>` for a trait impl.
fn label(seg: &Segmenter<'_, '_>, line: usize, symbol: &Symbol) -> String {
    match defs::detect(seg.lines.line(line), seg.lang).and_then(|d| d.implements) {
        Some(t) if symbol.kind == SymbolKind::Impl => format!("impl {t} for {}", symbol.name),
        _ => format!("{} {}", symbol.kind.as_str(), symbol.name),
    }
}

/// Further definitions at the level of the one on `line` in the same unit:
/// consecutive constants, module declarations, one-line functions. The
/// item at `out[at]` and each found one end where the next begins.
fn siblings(
    seg: &Segmenter<'_, '_>,
    line: usize,
    e: usize,
    depth: usize,
    parent: Option<&Symbol>,
    at: usize,
    out: &mut Vec<OutlineItem>,
) {
    let level = seg.info[line].depth_start;
    let mut prev = at;
    for i in line + 1..e {
        let li = seg.info[i];
        let text = seg.lines.line(i);
        if li.blank || li.in_literal || li.depth_start != level {
            continue;
        }
        if is_comment_or_attribute(text.trim_start(), seg.lang) {
            continue;
        }
        let Some(def) = defs::detect(text, seg.lang) else {
            continue;
        };
        if parent.is_some_and(|p| p.name == def.name) {
            continue;
        }
        // The definition's own comments and attributes belong to it.
        let mut start = i;
        while start > line + 1
            && is_comment_or_attribute(seg.lines.line(start - 1).trim_start(), seg.lang)
        {
            start -= 1;
        }
        out[prev].lines.1 = (start as u32).max(out[prev].lines.0);
        let in_type = parent.is_some_and(|p| p.kind.is_container() && p.kind != SymbolKind::Module);
        let kind = match def.kind {
            SymbolKind::Function if in_type => SymbolKind::Method,
            kind => kind,
        };
        let symbol = Symbol {
            name: def.name,
            kind,
        };
        out.push(OutlineItem {
            name: label(seg, i, &symbol),
            depth,
            lines: (start as u32 + 1, e as u32),
        });
        prev = out.len() - 1;
    }
}
