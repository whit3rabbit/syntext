//! Programmatic hook protocol handlers.

use std::io::{self, Read};

use serde_json::Value;

mod claude;
mod copilot;
mod cursor;
mod gemini;

const STDIN_CAP: usize = 1_048_576;

/// CLI entrypoint for `st __hook <target>`.
pub fn cmd_hook(target: &str) -> i32 {
    let input = match read_stdin_limited() {
        Ok(input) => input,
        Err(_) => return 0,
    };
    let st_program = current_st_program();
    let output = match target {
        "claude" => claude::response_from_str(&input, &st_program),
        "claude-grep-block" => claude::grep_block_response_from_str(&input),
        "cursor" => cursor::response_from_str(&input, &st_program),
        "copilot" => copilot::response_from_str(&input, &st_program),
        "gemini" => gemini::response_from_str(&input, &st_program),
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

fn read_stdin_limited() -> Result<String, String> {
    let mut input = String::new();
    io::stdin()
        .take((STDIN_CAP + 1) as u64)
        .read_to_string(&mut input)
        .map_err(|err| format!("st: failed to read hook stdin: {err}"))?;
    if input.len() > STDIN_CAP {
        return Err("st: hook stdin exceeds 1 MiB".to_string());
    }
    Ok(input)
}

fn current_st_program() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or_else(|| "st".to_string())
}

fn parse_json(input: &str) -> Option<Value> {
    if input.trim().is_empty() {
        return None;
    }
    serde_json::from_str(input).ok()
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
