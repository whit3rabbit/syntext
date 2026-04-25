//! Claude Code PreToolUse hook protocol.

use serde_json::{json, Value};

use crate::hook::core::rewrite::{find_index_root, rewrite_for_cwd};

use super::{hook_cwd, parse_json, ProtocolOutput};

pub(crate) fn response_from_str(input: &str, st_program: &str) -> Option<ProtocolOutput> {
    let input = parse_json(input)?;
    bash_rewrite_response(&input, st_program).map(ProtocolOutput::Json)
}

pub(crate) fn grep_block_response_from_str(input: &str) -> Option<ProtocolOutput> {
    let input = parse_json(input)?;
    grep_block_response(&input).map(ProtocolOutput::Json)
}

fn bash_rewrite_response(input: &Value, st_program: &str) -> Option<Value> {
    if input.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }

    let command = input
        .get("tool_input")
        .and_then(|tool_input| tool_input.get("command"))
        .and_then(Value::as_str)?;
    let cwd = hook_cwd(input);
    let rewritten = rewrite_for_cwd(command, &cwd, st_program)?;
    if rewritten == command {
        return None;
    }

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

fn grep_block_response(input: &Value) -> Option<Value> {
    if input.get("tool_name").and_then(Value::as_str) != Some("Grep") {
        return None;
    }

    let cwd = hook_cwd(input);
    find_index_root(&cwd)?;

    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "Use Bash(st \"pattern\" path) instead of the built-in Grep tool so syntext can use the index."
        }
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
    fn claude_bash_hook_returns_ask_with_updated_input() {
        let dir = indexed_repo();
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {
                "command": "rg parse_query src/",
                "description": "search"
            },
            "cwd": dir.path()
        });

        let ProtocolOutput::Json(output) =
            response_from_str(&input.to_string(), "/tmp/st").unwrap()
        else {
            panic!("expected JSON output");
        };
        let hook = &output["hookSpecificOutput"];
        assert_eq!(hook["hookEventName"], "PreToolUse");
        assert_eq!(hook["permissionDecision"], "ask");
        assert_eq!(hook["updatedInput"]["command"], "/tmp/st parse_query src/");
        assert_eq!(hook["updatedInput"]["description"], "search");
    }

    #[test]
    fn claude_bash_hook_is_silent_without_rewrite() {
        let dir = tempfile::TempDir::new().unwrap();
        let input = json!({
            "tool_name": "Bash",
            "tool_input": {
                "command": "rg parse_query src/"
            },
            "cwd": dir.path()
        });

        assert_eq!(response_from_str(&input.to_string(), "st"), None);
    }

    #[test]
    fn claude_hook_ignores_malformed_json() {
        assert_eq!(response_from_str("not json", "st"), None);
    }

    #[test]
    fn grep_block_denies_only_when_index_exists() {
        let indexed = indexed_repo();
        let input = json!({
            "tool_name": "Grep",
            "tool_input": { "pattern": "parse_query" },
            "cwd": indexed.path()
        });
        let ProtocolOutput::Json(output) =
            grep_block_response_from_str(&input.to_string()).unwrap()
        else {
            panic!("expected JSON output");
        };
        assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");

        let plain = tempfile::TempDir::new().unwrap();
        let input = json!({
            "tool_name": "Grep",
            "tool_input": { "pattern": "parse_query" },
            "cwd": plain.path()
        });
        assert_eq!(grep_block_response_from_str(&input.to_string()), None);
    }
}
