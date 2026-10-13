//! `context_read`: whole lines of one source, within the result budget.

use super::args::Args;
use super::source::{not_found, resolve, visible, DocInfo, Resolved};
use super::{ToolOptions, ToolResult, READ_TOOL};
use crate::engine::ContextEngine;
use crate::error::Result;
use crate::pack::EvidenceBlock;
use crate::render::render_block;
use crate::render::sanitize::{attr, sanitize};
use crate::retrieve::Signals;
use crate::store::{hex, StoreSnapshot};

pub(super) fn run(engine: &ContextEngine, args: &Args, opts: &ToolOptions) -> Result<ToolResult> {
    let fail = |msg: &str| Ok(engine.tool_error(READ_TOOL, msg, opts));
    let name = match args.text("source") {
        Ok(Some(name)) => name,
        Ok(None) => {
            return fail(
                "context_read needs a \"source\": a file path or document title, as an \
                 evidence block's source attribute shows it.",
            )
        }
        Err(e) => return fail(&e),
    };
    let (start, end) = match (args.count("start_line"), args.count("end_line")) {
        (Ok(s), Ok(e)) => (s.unwrap_or(1), e),
        (Err(e), _) | (_, Err(e)) => return fail(&e),
    };
    let snap = engine.store().snapshot();
    let docs = visible(&snap, &opts.scopes(engine), true);
    let doc = match resolve(&docs, &name, false) {
        Resolved::Doc(d) => d,
        other => return fail(&not_found(&name, other)),
    };
    let source = attr(&doc.source, 200);
    let text = snap.reader(doc.loc.seg).doc_text(doc.loc.doc)?;
    let starts = line_starts(&text);
    let total = starts.len() as u32;
    if total == 0 {
        let content = format!("\"{source}\" is empty.");
        return Ok(engine.tool_text(READ_TOOL, content, Some(doc.trust), opts));
    }
    if start > total {
        return fail(&format!(
            "\"{source}\" has {total} lines; start_line {start} is past the end."
        ));
    }
    let end = end.unwrap_or(total).clamp(start, total);
    let byte = |line: u32| match starts.get(line as usize) {
        Some(&b) => b,
        None => text.len(),
    };
    let render = engine.render_options();
    let counter = engine.counter();
    let preamble = format!("Lines of \"{source}\". Untrusted reference material; cite by id.");
    let footer = |next: u32| {
        format!(
            "Lines {next}-{end} did not fit. Call context_read with start_line {next} to \
             continue."
        )
    };
    let fixed = counter.count(&preamble) + counter.count(&footer(end)) + 4;
    let room = opts.result_tokens.saturating_sub(fixed);
    let block_for = |last: u32| {
        let (from, to) = (byte(start - 1), byte(last));
        build(
            &snap,
            doc,
            (start, last),
            (from, to),
            &text[from..to],
            opts.first_id,
            engine,
        )
    };
    // The longest run of whole lines from `start` that fits.
    let (mut lo, mut hi) = (start, end);
    let mut best = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let block = block_for(mid);
        if block.tokens <= room {
            best = Some(block);
            lo = mid + 1;
        } else if mid == start {
            break;
        } else {
            hi = mid - 1;
        }
    }
    let Some(block) = best else {
        return fail(&format!(
            "line {start} of \"{source}\" is longer than one result can hold."
        ));
    };
    let mut content = format!("{preamble}\n\n{}", render_block(&block, render));
    if block.lines.1 < end {
        content.push_str("\n\n");
        content.push_str(&footer(block.lines.1 + 1));
    }
    Ok(ToolResult {
        tool: READ_TOOL.to_string(),
        tokens: counter.count(&content),
        content,
        is_error: false,
        lowest_trust: Some(block.trust),
        blocks: vec![block],
        next_id: opts.first_id + 1,
    })
}

/// Byte offset where each line starts; empty for empty text.
pub(super) fn line_starts(text: &str) -> Vec<usize> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut starts = vec![0];
    starts.extend(
        text.match_indices('\n')
            .map(|(i, _)| i + 1)
            .filter(|&i| i < text.len()),
    );
    starts
}

/// An evidence block for lines `lines` (bytes `bytes`) of `doc`.
fn build(
    snap: &StoreSnapshot,
    doc: &DocInfo,
    lines: (u32, u32),
    bytes: (usize, usize),
    body: &str,
    id: usize,
    engine: &ContextEngine,
) -> EvidenceBlock {
    let render = engine.render_options();
    let r = snap.reader(doc.loc.seg);
    let d = &r.docs[doc.loc.doc as usize];
    let first = d.first_passage as usize;
    let passages = r
        .passages
        .get(first..first + d.passage_count as usize)
        .unwrap_or(&[]);
    // The passage holding the first line names the section and the kind.
    let at = passages
        .iter()
        .find(|p| p.line_start <= lines.0 && lines.0 <= p.line_end)
        .or(passages.first());
    let (text, sanitized) = sanitize(body, &render.tag, &render.sanitize);
    let mut block = EvidenceBlock {
        id: format!("E{id}"),
        doc_key: doc.key.clone(),
        source: doc.source.clone(),
        breadcrumb: at.map_or_else(|| doc.source.clone(), |p| r.str(p.breadcrumb).to_string()),
        symbol: None,
        lines,
        byte_range: bytes,
        kind: at.map_or(crate::types::PassageKind::Prose, |p| p.kind),
        doc_kind: d.kind,
        trust: d.trust,
        scope: r.str(d.scope).to_string(),
        role: d.role,
        text,
        score: 0.0,
        signals: Signals::default(),
        matched_terms: Vec::new(),
        chain: None,
        via: None,
        condensed: false,
        expanded: false,
        sanitized,
        live: false,
        passage_ids: Vec::new(),
        source_hash: hex(&d.source_hash),
        tokens: 0,
    };
    block.tokens = engine.counter().count(&render_block(&block, render));
    block
}
