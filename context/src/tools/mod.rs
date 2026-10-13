//! Agent tools: the model searches again, reads a source, and sees what the
//! corpus holds.
//!
//! The first evidence packet is chosen from the question alone. When it
//! falls short, as when the second hop of a question shares no words with
//! it or a follow-up needs the rest of a file, a model that can call tools
//! can look again in its own words. Three read-only tools:
//!
//! | Tool | Arguments | Returns |
//! |---|---|---|
//! | `context_search` | `query`, `max_results`? | ranked passages as evidence blocks |
//! | `context_read` | `source`, `start_line`?, `end_line`? | whole lines of one source |
//! | `context_outline` | `source`? | what the corpus, a directory, or a document holds |
//!
//! Results are untrusted evidence, like the packet: sanitized, in
//! `<evidence>` or `<outline>` elements, and within
//! [`ToolOptions::result_tokens`]. Scopes come from the application through
//! [`ToolOptions::scopes`], never from the model. No argument widens what
//! the user may read, and a source outside the granted scopes reads as
//! missing.

mod args;
mod outline;
mod read;
mod search;
mod source;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::engine::{ContextEngine, EvidencePacket};
use crate::error::{ContextError, Result};
use crate::pack::EvidenceBlock;
use crate::render::sanitize::attr;
use crate::types::{ScopeFilter, Trust};
use args::Args;

/// Name of the search tool.
pub const SEARCH_TOOL: &str = "context_search";
/// Name of the read tool.
pub const READ_TOOL: &str = "context_read";
/// Name of the outline tool.
pub const OUTLINE_TOOL: &str = "context_outline";
/// Smallest [`ToolOptions::result_tokens`] accepted: room for a short
/// message telling the model what went wrong.
pub const MIN_RESULT_TOKENS: usize = 64;

/// A tool to offer the model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDefinition {
    /// Function name.
    pub name: &'static str,
    /// What the tool does, written for the model.
    pub description: &'static str,
    /// JSON Schema of the arguments.
    pub parameters: Value,
}

impl ToolDefinition {
    /// The OpenAI form that most local inference servers and chat templates
    /// accept: `{"type": "function", "function": {name, description,
    /// parameters}}`.
    pub fn openai(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }

    /// The Model Context Protocol form: `{name, description, inputSchema}`.
    pub fn mcp(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.parameters,
        })
    }
}

/// The three tools, in the order to offer them.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: SEARCH_TOOL,
            description: "Search the project's files, documents, and earlier conversation. \
                Returns the passages that best match the query as <evidence> blocks, each with \
                an id to cite, its source, and its line range. Call it when the evidence you \
                have does not answer the question. Search for the names, identifiers, or \
                phrases the answer would contain, not for the question itself.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Words, names, or identifiers to look for."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 10,
                        "description": "Most passages to return."
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: READ_TOOL,
            description: "Read lines of one source: a file path or document title, as an \
                evidence block's source attribute shows it. Returns whole lines. A result \
                that stops early says where to continue.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "File path or document title, as shown in evidence."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line to read (default 1)."
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Last line to read (default: the end)."
                    }
                },
                "required": ["source"]
            }),
        },
        ToolDefinition {
            name: OUTLINE_TOOL,
            description: "Show what the sources contain. Without a source: the documents \
                and directories available. With a directory such as \"src/\": the documents \
                in it. With a file path or document title: its sections and definitions \
                with line ranges, to read with context_read.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Optional directory, file path, or document title."
                    }
                }
            }),
        },
    ]
}

/// A document range the model has already seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shown {
    /// Document key.
    pub doc_key: String,
    /// 1-based first and last line.
    pub lines: (u32, u32),
}

/// Settings for tool calls. They come from the application, never from the
/// model's arguments.
#[derive(Debug, Clone)]
pub struct ToolOptions {
    /// Scopes the tools may read; `None` uses the engine's retrieval scopes.
    pub scopes: Option<ScopeFilter>,
    /// Tokens one result may use, by the engine's token counter (at least
    /// [`MIN_RESULT_TOKENS`]).
    pub result_tokens: usize,
    /// Most passages one search returns, whatever the model asks for.
    pub max_results: usize,
    /// Number of the first citation id in a result (`E<n>`). Keep it past
    /// every id already in the conversation, so citations stay unique:
    /// [`record_packet`](Self::record_packet) and
    /// [`record`](Self::record) do that.
    pub first_id: usize,
    /// Ranges already in the conversation. Search skips passages that lie
    /// inside one, so a follow-up search returns something new.
    pub shown: Vec<Shown>,
}

impl Default for ToolOptions {
    fn default() -> Self {
        ToolOptions {
            scopes: None,
            result_tokens: 1024,
            max_results: 6,
            first_id: 1,
            shown: Vec::new(),
        }
    }
}

impl ToolOptions {
    /// Continue after `packet`: its blocks count as shown, and ids continue
    /// after its own.
    pub fn record_packet(&mut self, packet: &EvidencePacket) {
        self.record_blocks(&packet.blocks);
    }

    /// Continue after a tool result.
    pub fn record(&mut self, result: &ToolResult) {
        self.record_blocks(&result.blocks);
        self.first_id = self.first_id.max(result.next_id);
    }

    fn record_blocks(&mut self, blocks: &[EvidenceBlock]) {
        for b in blocks {
            // A condensed block left lines out; a search may still need them.
            if !b.condensed {
                self.shown.push(Shown {
                    doc_key: b.doc_key.clone(),
                    lines: b.lines,
                });
            }
            if let Some(n) = b.id.strip_prefix('E').and_then(|n| n.parse::<usize>().ok()) {
                self.first_id = self.first_id.max(n + 1);
            }
        }
    }

    /// Whether lines `lines` of `doc_key` lie inside a range already shown.
    pub(crate) fn covers(&self, doc_key: &str, lines: (u32, u32)) -> bool {
        self.shown
            .iter()
            .any(|s| s.doc_key == doc_key && s.lines.0 <= lines.0 && lines.1 <= s.lines.1)
    }

    fn scopes(&self, engine: &ContextEngine) -> ScopeFilter {
        self.scopes
            .clone()
            .unwrap_or_else(|| engine.retrieve_options().scopes.clone())
    }
}

/// The output of one tool call.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    /// The tool that ran.
    pub tool: String,
    /// Text to return to the model as the tool's output.
    pub content: String,
    /// Tokens of `content`.
    pub tokens: usize,
    /// The call was malformed, or named a source the tools cannot read.
    /// `content` says why, so the model can correct the call.
    pub is_error: bool,
    /// Evidence blocks in `content` (search and read results).
    pub blocks: Vec<EvidenceBlock>,
    /// Least-trusted source in `content`. Hosts can gate side-effecting
    /// tools on it, as with [`EvidencePacket::lowest_trust`].
    pub lowest_trust: Option<Trust>,
    /// One more than the highest citation id in `content`.
    pub next_id: usize,
}

impl ContextEngine {
    /// Run a tool call the model made. `arguments` is the call's JSON as
    /// the model wrote it.
    ///
    /// A malformed call, an unknown tool, or a source the tools cannot read
    /// is an `Ok` result with `is_error` set, to hand back to the model so
    /// it can try again. `Err` is kept for failures of the store itself and
    /// for `opts.result_tokens` below [`MIN_RESULT_TOKENS`]. The result
    /// never exceeds `opts.result_tokens`.
    pub fn call_tool(&self, name: &str, arguments: &str, opts: &ToolOptions) -> Result<ToolResult> {
        if opts.result_tokens < MIN_RESULT_TOKENS {
            return Err(ContextError::invalid(format!(
                "result_tokens must be at least {MIN_RESULT_TOKENS}"
            )));
        }
        let result = self.dispatch(name, arguments, opts)?;
        if result.tokens <= opts.result_tokens {
            return Ok(result);
        }
        // Messages echo the model's arguments and source names, so even a
        // fixed message can run long.
        let msg = "the reply did not fit in the space allowed. Ask for less: a narrower \
                   query, a smaller directory, or a shorter line range.";
        Ok(self.tool_error(name, msg, opts))
    }

    fn dispatch(&self, name: &str, arguments: &str, opts: &ToolOptions) -> Result<ToolResult> {
        let args = match Args::parse(arguments) {
            Ok(args) => args,
            Err(msg) => return Ok(self.tool_error(name, &msg, opts)),
        };
        match name {
            SEARCH_TOOL => search::run(self, &args, opts),
            READ_TOOL => read::run(self, &args, opts),
            OUTLINE_TOOL => outline::run(self, &args, opts),
            _ => {
                let msg = format!(
                    "there is no tool named \"{}\". The tools are {SEARCH_TOOL}, {READ_TOOL}, \
                     and {OUTLINE_TOOL}.",
                    attr(name, 64)
                );
                Ok(self.tool_error(name, &msg, opts))
            }
        }
    }

    /// A result telling the model what was wrong with its call.
    pub(crate) fn tool_error(&self, tool: &str, message: &str, opts: &ToolOptions) -> ToolResult {
        let content = format!("Error: {message}");
        ToolResult {
            tool: attr(tool, 64),
            tokens: self.counter().count(&content),
            content,
            is_error: true,
            blocks: Vec::new(),
            lowest_trust: None,
            next_id: opts.first_id,
        }
    }

    /// A plain-text result.
    pub(crate) fn tool_text(
        &self,
        tool: &str,
        content: String,
        lowest_trust: Option<Trust>,
        opts: &ToolOptions,
    ) -> ToolResult {
        ToolResult {
            tool: tool.to_string(),
            tokens: self.counter().count(&content),
            content,
            is_error: false,
            blocks: Vec::new(),
            lowest_trust,
            next_id: opts.first_id,
        }
    }
}

#[cfg(test)]
mod tests;
