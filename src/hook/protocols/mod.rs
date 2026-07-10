//! Programmatic hook protocol handlers.

use serde_json::Value;

mod claude;
mod copilot;
mod cursor;
mod gemini;

const STDIN_CAP: usize = 1_048_576;

/// CLI entrypoint for `st __hook <target>`.
pub fn cmd_hook(target: &str) -> i32 {
    let input = read_stdin_json();
    let st_program = current_st_program();
    let output = match target {
        "claude" => input
            .as_ref()
            .and_then(|v| claude::response(v, &st_program)),
        "claude-grep-block" => input.as_ref().and_then(claude::grep_block_response),
        "cursor" => Some(cursor::response(input.as_ref(), &st_program)),
        "copilot" => input
            .as_ref()
            .and_then(|v| copilot::response(v, &st_program)),
        "gemini" => input
            .as_ref()
            .and_then(|v| gemini::response(v, &st_program)),
        other => {
            eprintln!("st: unsupported hook target '{other}'");
            return 2;
        }
    };

    if let Some(output) = output {
        match output {
            ProtocolOutput::Json(value) => {
                if let Ok(line) = serde_json::to_string(&value) {
                    println!("{line}");
                }
            }
            ProtocolOutput::Literal(value) => println!("{value}"),
        }
    }
    0
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProtocolOutput {
    Json(Value),
    Literal(String),
}

/// Read one JSON value from stdin without depending on EOF.
///
/// The streaming deserializer returns as soon as a complete value is parsed, so
/// a caller that writes the payload but keeps the stdin pipe open (or an
/// interactive TTY with no input) cannot wedge the hook — the previous
/// `read_to_string`-to-EOF blocked forever in exactly that case. Returns `None`
/// for a TTY, empty/unparseable input, or a payload exceeding the 1 MiB `take`
/// cap; the caller treats `None` as silent passthrough (cursor emits `{}`).
fn read_stdin_json() -> Option<Value> {
    use serde::Deserialize;
    use std::io::{IsTerminal, Read};

    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let limited = stdin.lock().take((STDIN_CAP + 1) as u64);
    let mut de = serde_json::Deserializer::from_reader(limited);
    Value::deserialize(&mut de).ok()
}

fn current_st_program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or_else(|| "st".to_string())
}

fn hook_cwd(input: &Value) -> std::path::PathBuf {
    use std::path::{Path, PathBuf};

    input
        .get("cwd")
        .or_else(|| input.get("working_dir"))
        .and_then(Value::as_str)
        .map(Path::new)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
