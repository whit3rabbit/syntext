//! Cursor Agent native hook protocol.

use serde_json::{json, Value};

use crate::hook::core::rewrite::rewrite_for_cwd;

use super::{hook_cwd, ProtocolOutput};

pub(crate) fn response(input: Option<&Value>, st_program: &str) -> ProtocolOutput {
    match input {
        Some(v) => ProtocolOutput::Json(build_response(v, st_program)),
        None => ProtocolOutput::Literal("{}".to_string()),
    }
}

fn build_response(input: &Value, st_program: &str) -> Value {
    let Some(command) = input
        .get("tool_input")
        .and_then(|tool_input| tool_input.get("command"))
        .and_then(Value::as_str)
    else {
        return json!({});
    };
    let cwd = hook_cwd(input);
    let Some(rewritten) = rewrite_for_cwd(command, &cwd, st_program) else {
        return json!({});
    };
    if rewritten == command {
        return json!({});
    }
    json!({
        "permission": "ask",
        "updated_input": {
            "command": rewritten
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn cursor_rewrite_uses_flat_shape() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".syntext")).unwrap();
        let input = json!({
            "tool_input": { "command": "rg parse_query src/" },
            "cwd": dir.path()
        });
        let ProtocolOutput::Json(output) = response(Some(&input), "/tmp/st") else {
            panic!("expected JSON output");
        };
        assert_eq!(output["permission"], "ask");
        assert_eq!(
            output["updated_input"]["command"],
            "/tmp/st -- parse_query src/"
        );
    }

    #[test]
    fn cursor_passthrough_is_empty_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let input = json!({
            "tool_input": { "command": "rg parse_query src/" },
            "cwd": dir.path()
        });
        let ProtocolOutput::Json(output) = response(Some(&input), "st") else {
            panic!("expected JSON output");
        };
        assert_eq!(output, json!({}));
    }
}
