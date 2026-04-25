//! Shared JSON settings patch helpers.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use super::files;
use super::shell::{self, ShellItem};

pub(crate) fn read_json_object(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&text).map_err(|err| {
        format!(
            "st: refusing to patch malformed JSON in {}: {err}",
            path.display()
        )
    })?;
    if !value.is_object() {
        return Err(format!(
            "st: refusing to patch {}: top-level JSON value must be an object",
            path.display()
        ));
    }
    Ok(value)
}

pub(crate) fn write_json_if_changed(path: &Path, value: &Value) -> Result<bool, String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|err| format!("st: failed to serialize JSON: {err}"))?;
    text.push('\n');
    if path.exists() {
        let existing = fs::read_to_string(path)
            .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
        if existing == text {
            return Ok(false);
        }
    }
    files::write_text_if_changed(path, &text)
}

pub(crate) fn hook_command(st_program: &str, target: &str) -> String {
    format!("{} __hook {target}", shell::shell_quote(st_program))
}

pub(crate) fn is_syntext_hook_command(command: &str, target: Option<&str>) -> bool {
    let Ok(parsed) = shell::parse(command) else {
        return false;
    };
    let [ShellItem::Command(words)] = parsed.items.as_slice() else {
        return false;
    };
    if words.len() != 3 || words[1].text != "__hook" {
        return false;
    }
    if let Some(target) = target {
        if words[2].text != target {
            return false;
        }
    } else if !matches!(
        words[2].text.as_str(),
        "claude" | "claude-grep-block" | "cursor" | "copilot" | "gemini"
    ) {
        return false;
    }
    let program = Path::new(&words[0].text)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(words[0].text.as_str());
    program == "st" || program == "st.exe"
}

pub(crate) fn add_grouped_hook(
    root: &mut Value,
    event: &str,
    matcher: &str,
    command: &str,
) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "st: settings root must be an object".to_string())?;
    let hooks = root_obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| "st: settings.hooks must be an object".to_string())?;
    let event_hooks = hooks_obj.entry(event).or_insert_with(|| json!([]));
    let event_hooks = event_hooks
        .as_array_mut()
        .ok_or_else(|| format!("st: settings.hooks.{event} must be an array"))?;
    let handler = json!({ "type": "command", "command": command });

    if let Some(group) = event_hooks
        .iter_mut()
        .find(|group| group.get("matcher").and_then(Value::as_str) == Some(matcher))
    {
        let group_obj = group
            .as_object_mut()
            .ok_or_else(|| "st: hook matcher group must be an object".to_string())?;
        let hooks = group_obj.entry("hooks").or_insert_with(|| json!([]));
        let hooks = hooks
            .as_array_mut()
            .ok_or_else(|| "st: hook matcher group hooks must be an array".to_string())?;
        if !hooks.contains(&handler) {
            hooks.push(handler);
        }
        return Ok(());
    }

    event_hooks.push(json!({ "matcher": matcher, "hooks": [handler] }));
    Ok(())
}

pub(crate) fn remove_grouped_hooks(root: &mut Value, event: &str) -> Result<usize, String> {
    let Some(event_hooks) = root.get_mut("hooks").and_then(|hooks| hooks.get_mut(event)) else {
        return Ok(0);
    };
    let event_hooks = event_hooks
        .as_array_mut()
        .ok_or_else(|| format!("st: settings.hooks.{event} must be an array"))?;
    let mut removed = 0;
    let mut empty_groups = Vec::new();
    for (index, group) in event_hooks.iter_mut().enumerate() {
        let Some(hooks) = group.get_mut("hooks") else {
            continue;
        };
        let hooks = hooks
            .as_array_mut()
            .ok_or_else(|| "st: hook matcher group hooks must be an array".to_string())?;
        let before = hooks.len();
        hooks.retain(|handler| {
            let command = handler.get("command").and_then(Value::as_str);
            !command.is_some_and(|command| is_syntext_hook_command(command, None))
        });
        let removed_here = before - hooks.len();
        removed += removed_here;
        if removed_here > 0 && hooks.is_empty() {
            empty_groups.push(index);
        }
    }
    for index in empty_groups.into_iter().rev() {
        event_hooks.remove(index);
    }
    Ok(removed)
}

pub(crate) fn has_grouped_hook(root: &Value, event: &str, target: &str) -> Result<bool, String> {
    let Some(event_hooks) = root.get("hooks").and_then(|hooks| hooks.get(event)) else {
        return Ok(false);
    };
    let event_hooks = event_hooks
        .as_array()
        .ok_or_else(|| format!("st: settings.hooks.{event} must be an array"))?;
    for group in event_hooks {
        let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        if hooks.iter().any(|handler| {
            handler
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| is_syntext_hook_command(command, Some(target)))
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}
