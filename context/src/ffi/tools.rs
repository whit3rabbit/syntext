//! `extern "C"` entry points for the agent tools: their definitions, and
//! running a call the model made.

use std::os::raw::c_char;

use serde::Deserialize;
use serde_json::Value;
use syntext::ffi::{cstr, ffi_ptr, owned_json, syntext_error, FfiError};

use super::store::req_json;
use super::{borrow_engine, ffi_error, syntext_context_engine};
use crate::tools::{tool_definitions, Shown, ToolOptions};
use crate::types::{Scope, ScopeFilter};

/// The tool definitions as a JSON array. `format` (nullable): `"openai"`
/// (default), `{"type": "function", "function": {name, description,
/// parameters}}` per tool; or `"mcp"`, `{name, description, inputSchema}`.
#[no_mangle]
pub extern "C" fn syntext_context_tool_definitions(
    format: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let defs = tool_definitions();
        let out: Vec<Value> = match cstr(format)?.unwrap_or("openai") {
            "openai" => defs.iter().map(|d| d.openai()).collect(),
            "mcp" => defs.iter().map(|d| d.mcp()).collect(),
            other => {
                return Err(FfiError::invalid_arg(format!(
                    "unknown tool format {other:?}; use \"openai\" or \"mcp\""
                )))
            }
        };
        owned_json(&out)
    })
}

/// `request_json` of `syntext_context_tool_call`.
#[derive(Debug, Deserialize)]
struct ToolCallDto {
    name: String,
    /// The model's arguments: a JSON string as the model wrote it, or the
    /// object already parsed.
    #[serde(default)]
    arguments: Value,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    result_tokens: Option<usize>,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    first_id: Option<usize>,
    #[serde(default)]
    shown: Vec<Shown>,
}

/// Run a tool call the model made. `request_json`: `{"name", "arguments",
/// "scopes"?, "result_tokens"?, "max_results"?, "first_id"?, "shown"?:
/// [{"doc_key", "lines": [first, last]}]}`. `arguments` is the call's JSON
/// as the model wrote it (a string) or as an object. Scopes and limits come
/// from the app, never from the model. Returns `{"tool", "content",
/// "tokens", "is_error", "blocks", "lowest_trust", "next_id"}`: a malformed
/// call comes back with `is_error` and a message for the model, not as an
/// error here.
#[no_mangle]
pub extern "C" fn syntext_context_tool_call(
    engine: *const syntext_context_engine,
    request_json: *const c_char,
    err_out: *mut *mut syntext_error,
) -> *mut c_char {
    ffi_ptr(err_out, || unsafe {
        let engine = borrow_engine(engine)?;
        let req: ToolCallDto = req_json(request_json, "request")?;
        let d = ToolOptions::default();
        let scopes = match req.scopes {
            Some(list) => Some(ScopeFilter::only(
                list.iter()
                    .map(|s| Scope::new(s.as_str()))
                    .collect::<crate::Result<Vec<_>>>()
                    .map_err(ffi_error)?,
            )),
            None => None,
        };
        let opts = ToolOptions {
            scopes,
            result_tokens: req.result_tokens.unwrap_or(d.result_tokens),
            max_results: req.max_results.unwrap_or(d.max_results).max(1),
            first_id: req.first_id.unwrap_or(d.first_id).max(1),
            shown: req.shown,
        };
        let arguments = match &req.arguments {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let result = engine
            .call_tool(&req.name, &arguments, &opts)
            .map_err(ffi_error)?;
        owned_json(&result)
    })
}
