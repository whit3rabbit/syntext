//! Gemini CLI BeforeTool hook protocol.

use serde_json::{json, Value};

use crate::hook::core::rewrite::rewrite_for_cwd;

use super::{hook_cwd, parse_json, ProtocolOutput};

pub(crate) fn response_from_str(input: &str, st_program: &str) -> Option<ProtocolOutput> {
    let input = parse_json(input)?;
    Some(ProtocolOutput::Json(response(&input, st_program)))
}

fn response(input: &Value, st_program: &str) -> Value {
    if input.get("tool_name").and_then(Value::as_str) != Some("run_shell_command") {
        return json!({ "decision": "allow" });
    }
    let Some(command) = input
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .filter(|command| !command.is_empty())
    else {
        return json!({ "decision": "allow" });
    };
    let cwd = hook_cwd(input);
    let Some(rewritten) = rewrite_for_cwd(command, &cwd, st_program) else {
        return json!({ "decision": "allow" });
    };
    json!({
        "decision": "allow",
        "hookSpecificOutput": {
            "tool_input": {
                "command": rewritten
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn gemini_rewrite_uses_tool_input_shape() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".syntext")).unwrap();
        let input = json!({
            "tool_name": "run_shell_command",
            "tool_input": { "command": "rg parse_query src/" },
            "cwd": dir.path()
        });
        let ProtocolOutput::Json(output) =
            response_from_str(&input.to_string(), "/tmp/st").unwrap()
        else {
            panic!("expected JSON output");
        };
        assert_eq!(output["decision"], "allow");
        assert_eq!(
            output["hookSpecificOutput"]["tool_input"]["command"],
            "/tmp/st parse_query src/"
        );
    }

    #[test]
    fn gemini_passthrough_allows() {
        let input = json!({ "tool_name": "read_file" });
        let ProtocolOutput::Json(output) = response_from_str(&input.to_string(), "st").unwrap()
        else {
            panic!("expected JSON output");
        };
        assert_eq!(output, json!({ "decision": "allow" }));
    }
}
