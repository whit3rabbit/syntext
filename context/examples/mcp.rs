//! A Model Context Protocol server over stdio that gives an agent the
//! context tools for one directory: `context_search`, `context_read`, and
//! `context_outline`.
//!
//! ```sh
//! cargo run -p syntext-context --release --example mcp -- <dir> \
//!     [--store <dir>] [--result-tokens 1500] [--syntext]
//! ```
//!
//! Register the built binary with an MCP client as a stdio server, for
//! example `{"command": "target/release/examples/mcp", "args":
//! ["/path/to/project"]}`. Messages are newline-delimited JSON-RPC 2.0 on
//! stdin and stdout; logs go to stderr. The directory is synced once at
//! start.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use syntext_context::tools::{tool_definitions, ToolOptions};
use syntext_context::{ContextEngine, ContextStore, DirectorySource, StoreOptions, SyntextSource};

/// Protocol versions this server speaks, newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

struct Args {
    root: PathBuf,
    store: PathBuf,
    result_tokens: usize,
    syntext: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut root = None;
    let mut store = None;
    let mut result_tokens = 1500;
    let mut syntext = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--store" => store = Some(PathBuf::from(it.next().ok_or("--store needs a value")?)),
            "--result-tokens" => {
                let v = it.next().ok_or("--result-tokens needs a value")?;
                result_tokens = v.parse().map_err(|_| format!("bad --result-tokens {v}"))?;
            }
            "--syntext" => syntext = true,
            _ if root.is_none() => root = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    let root = root.ok_or("usage: mcp <dir> [--store DIR] [--result-tokens N] [--syntext]")?;
    let store = store.unwrap_or_else(|| {
        let name: String = root
            .canonicalize()
            .unwrap_or_else(|_| root.clone())
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        std::env::temp_dir().join("syntext-context").join(name)
    });
    Ok(Args {
        root,
        store,
        result_tokens,
        syntext,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    let store = Arc::new(ContextStore::open_or_create(
        &args.store,
        StoreOptions::default(),
    )?);
    let sync = store.sync_directory(&DirectorySource::new("repo", &args.root)?)?;
    store.maybe_compact()?;
    eprintln!(
        "mcp: {} documents ({} scanned, +{} ~{} -{})",
        store.stats().documents,
        sync.scanned,
        sync.added,
        sync.modified,
        sync.removed
    );
    let mut builder = ContextEngine::builder(Arc::clone(&store));
    let mut _index = None;
    if args.syntext {
        let mut cfg = syntext::Config::new(args.store.join("syntext-index"), args.root.clone());
        cfg.auto_update = false;
        let index = match syntext::index::Index::open(cfg.clone()) {
            Ok(index) => index,
            Err(_) => syntext::index::Index::build(cfg)?,
        };
        let index = Arc::new(index);
        builder = builder.syntext(SyntextSource::new(Arc::clone(&index), "repo")?);
        _index = Some(index);
    }
    let engine = builder.build();
    let mut tools = ToolOptions {
        result_tokens: args.result_tokens,
        ..ToolOptions::default()
    };

    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&engine, &mut tools, &msg),
            Err(_) => Some(error(Value::Null, -32700, "parse error")),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// The reply to one message; `None` for notifications.
fn handle(engine: &ContextEngine, tools: &mut ToolOptions, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match msg.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "syntext-context", "version": env!("CARGO_PKG_VERSION")},
            })
        }
        "ping" => json!({}),
        "tools/list" => {
            let defs: Vec<Value> = tool_definitions().iter().map(|d| d.mcp()).collect();
            json!({ "tools": defs })
        }
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let arguments = params
                .get("arguments")
                .map_or(String::new(), Value::to_string);
            let (text, is_error) = match engine.call_tool(name, &arguments, tools) {
                Ok(r) => {
                    // Keep citation ids unique for the whole session.
                    tools.first_id = tools.first_id.max(r.next_id);
                    (r.content, r.is_error)
                }
                Err(e) => (format!("Error: {e}"), true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ => {
            return id.map(|id| error(id, -32601, "method not found"));
        }
    };
    id.map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
