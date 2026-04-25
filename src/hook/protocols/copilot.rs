//! GitHub Copilot VS Code Chat and CLI hook protocol.

use serde_json::{json, Value};

use crate::hook::core::rewrite::rewrite_for_cwd;

use super::{hook_cwd, parse_json, ProtocolOutput};

enum CopilotFormat {
    VsCode { command: String },
    Cli { command: String },
    PassThrough,
}

pub(crate) fn response_from_str(input: &str, st_program: &str) -> Option<ProtocolOutput> {
    let input = parse_json(input)?;
    match detect_format(&input) {
        CopilotFormat::VsCode { command } => vscode_response(&input, &command, st_program),
        CopilotFormat::Cli { command } => cli_response(&input, &command, st_program),
        CopilotFormat::PassThrough => None,
    }
    .map(ProtocolOutput::Json)
}

fn detect_format(input: &Value) -> CopilotFormat {
    if let Some(tool_name) = input.get("tool_name").and_then(Value::as_str) {
        if matches!(tool_name, "runTerminalCommand" | "Bash" | "bash") {
            if let Some(command) = input
                .pointer("/tool_input/command")
                .and_then(Value::as_str)
                .filter(|command| !command.is_empty())
            {
                return CopilotFormat::VsCode {
                    command: command.to_string(),
                };
            }
        }
        return CopilotFormat::PassThrough;
    }

    if input.get("toolName").and_then(Value::as_str) == Some("bash") {
        if let Some(tool_args) = input.get("toolArgs").and_then(Value::as_str) {
            if let Ok(tool_args) = serde_json::from_str::<Value>(tool_args) {
                if let Some(command) = tool_args
                    .get("command")
                    .and_then(Value::as_str)
                    .filter(|command| !command.is_empty())
                {
                    return CopilotFormat::Cli {
                        command: command.to_string(),
                    };
                }
            }
        }
    }

    CopilotFormat::PassThrough
}

fn vscode_response(input: &Value, command: &str, st_program: &str) -> Option<Value> {
    let cwd = hook_cwd(input);
    let rewritten = rewrite_for_cwd(command, &cwd, st_program)?;
    let mut updated_input = input.get("tool_input")?.clone();
    updated_input
        .as_object_mut()?
        .insert("command".to_string(), Value::String(rewritten));
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "ask",
            "permissionDecisionReason": "syntext rewrote rg/grep to st",
            "updatedInput": updated_input
        }
    }))
}

fn cli_response(input: &Value, command: &str, st_program: &str) -> Option<Value> {
    let cwd = hook_cwd(input);
    let rewritten = rewrite_for_cwd(command, &cwd, st_program)?;
    Some(json!({
        "permissionDecision": "deny",
        "permissionDecisionReason": format!(
            "Use `{}` instead so syntext can use the index.",
            rewritten
        )
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn indexed_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".syntext")).unwrap();
        dir
    }

    #[test]
    fn copilot_vscode_uses_updated_input() {
        let dir = indexed_repo();
        let input = json!({
            "tool_name": "runTerminalCommand",
            "tool_input": { "command": "rg parse_query src/", "timeout": 5 },
            "cwd": dir.path()
        });
        let ProtocolOutput::Json(output) =
            response_from_str(&input.to_string(), "/tmp/st").unwrap()
        else {
            panic!("expected JSON output");
        };
        assert_eq!(
            output["hookSpecificOutput"]["updatedInput"]["command"],
            "/tmp/st parse_query src/"
        );
        assert_eq!(output["hookSpecificOutput"]["updatedInput"]["timeout"], 5);
    }

    #[test]
    fn copilot_cli_uses_deny_with_suggestion() {
        let dir = indexed_repo();
        let args = serde_json::to_string(&json!({ "command": "rg parse_query src/" })).unwrap();
        let input = json!({ "toolName": "bash", "toolArgs": args, "cwd": dir.path() });
        let ProtocolOutput::Json(output) =
            response_from_str(&input.to_string(), "/tmp/st").unwrap()
        else {
            panic!("expected JSON output");
        };
        assert_eq!(output["permissionDecision"], "deny");
        assert!(output["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("/tmp/st parse_query src/"));
    }
}
