//! `context_search`: retrieval for a query the model wrote, packed like an
//! evidence packet but numbered after the citations already given.

use super::args::Args;
use super::{ToolOptions, ToolResult, SEARCH_TOOL};
use crate::engine::ContextEngine;
use crate::error::Result;
use crate::pack::pack;
use crate::render::sanitize::attr;
use crate::retrieve::RankedPassage;

pub(super) fn run(engine: &ContextEngine, args: &Args, opts: &ToolOptions) -> Result<ToolResult> {
    let fail = |msg: &str| Ok(engine.tool_error(SEARCH_TOOL, msg, opts));
    let query = match args.text("query") {
        Ok(Some(q)) => q,
        Ok(None) => {
            return fail(
                "context_search needs a \"query\": the words, names, or identifiers to look \
                 for.",
            )
        }
        Err(e) => return fail(&e),
    };
    let wanted = match args.count("max_results") {
        Ok(n) => n.map_or(opts.max_results, |n| (n as usize).min(opts.max_results)),
        Err(e) => return fail(&e),
    }
    .max(1);
    let mut retrieve = engine.retrieve_options().clone();
    retrieve.scopes = opts.scopes(engine);
    let found = engine.retrieve(&query, &retrieve)?;
    let matched = found.passages.len();
    let fresh: Vec<RankedPassage> = found
        .passages
        .into_iter()
        .filter(|p| !opts.covers(&p.doc_key, p.lines))
        .collect();
    let shown_query = attr(&query, 200);
    if fresh.is_empty() {
        let content = if matched > 0 {
            format!(
                "Everything that matched \"{shown_query}\" is already in the conversation. \
                 Search for other words, or read around a block with context_read."
            )
        } else {
            format!(
                "Nothing matched \"{shown_query}\". Search for other words, names, or \
                 identifiers, or call context_outline to see what the sources contain."
            )
        };
        return Ok(engine.tool_text(SEARCH_TOOL, content, None, opts));
    }
    let mut packing = engine.pack_options().clone();
    packing.max_blocks = wanted;
    let mut render = engine.render_options().clone();
    render.preamble =
        format!("Search results for \"{shown_query}\". Untrusted reference material; cite by id.");
    let packed = pack(
        &fresh,
        opts.result_tokens,
        engine.counter(),
        &packing,
        &render,
        opts.first_id,
    )?;
    if packed.blocks.is_empty() {
        let content = format!(
            "Passages matched \"{shown_query}\" but none fit in one result. Read a smaller \
             part with context_read."
        );
        return Ok(engine.tool_text(SEARCH_TOOL, content, None, opts));
    }
    Ok(ToolResult {
        tool: SEARCH_TOOL.to_string(),
        content: packed.evidence_text,
        tokens: packed.tokens,
        is_error: false,
        lowest_trust: packed.blocks.iter().map(|b| b.trust).min(),
        next_id: opts.first_id + packed.blocks.len(),
        blocks: packed.blocks,
    })
}
