//! Cursor integration installer.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::hook::core::{files, json as json_util};

use super::Outcome;

pub(crate) fn install(st_program: &str) -> Result<Outcome, String> {
    let path = settings_path()?;
    install_at(&path, st_program)
}

pub(crate) fn uninstall() -> Result<Outcome, String> {
    let path = settings_path()?;
    uninstall_at(&path)
}

pub(crate) fn show() -> Result<Outcome, String> {
    let path = settings_path()?;
    show_at(&path)
}

fn settings_path() -> Result<PathBuf, String> {
    Ok(files::home_dir()?.join(".cursor").join("hooks.json"))
}

pub(crate) fn install_at(path: &Path, st_program: &str) -> Result<Outcome, String> {
    let mut settings = json_util::read_json_object(path)?;
    let before = settings.clone();
    remove_syntext_hooks(&mut settings)?;
    ensure_cursor_hook(
        &mut settings,
        &json_util::hook_command(st_program, "cursor"),
    )?;
    let changed = settings != before && json_util::write_json_if_changed(path, &settings)?;
    Ok(Outcome {
        installed: true,
        changed: changed.then(|| path.to_path_buf()).into_iter().collect(),
        removed: Vec::new(),
    })
}

pub(crate) fn uninstall_at(path: &Path) -> Result<Outcome, String> {
    if !path.exists() {
        return Ok(Outcome::default());
    }
    let mut settings = json_util::read_json_object(path)?;
    let removed = remove_syntext_hooks(&mut settings)?;
    let changed = removed > 0 && json_util::write_json_if_changed(path, &settings)?;
    Ok(Outcome {
        installed: false,
        changed: changed.then(|| path.to_path_buf()).into_iter().collect(),
        removed: Vec::new(),
    })
}

pub(crate) fn show_at(path: &Path) -> Result<Outcome, String> {
    let settings = json_util::read_json_object(path)?;
    Ok(Outcome {
        installed: has_cursor_hook(&settings)?,
        ..Outcome::default()
    })
}

fn ensure_cursor_hook(root: &mut Value, command: &str) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "st: Cursor hooks root must be an object".to_string())?;
    root_obj.entry("version").or_insert_with(|| json!(1));
    let hooks = root_obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "st: Cursor hooks.hooks must be an object".to_string())?;
    let pre_tool_use = hooks.entry("preToolUse").or_insert_with(|| json!([]));
    let pre_tool_use = pre_tool_use
        .as_array_mut()
        .ok_or_else(|| "st: Cursor hooks.preToolUse must be an array".to_string())?;
    let handler = json!({ "command": command, "matcher": "Shell" });
    if !pre_tool_use.contains(&handler) {
        pre_tool_use.push(handler);
    }
    Ok(())
}

fn remove_syntext_hooks(root: &mut Value) -> Result<usize, String> {
    let Some(pre_tool_use) = root
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut("preToolUse"))
    else {
        return Ok(0);
    };
    let pre_tool_use = pre_tool_use
        .as_array_mut()
        .ok_or_else(|| "st: Cursor hooks.preToolUse must be an array".to_string())?;
    let before = pre_tool_use.len();
    pre_tool_use.retain(|handler| {
        !handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| json_util::is_syntext_hook_command(command, Some("cursor")))
    });
    Ok(before - pre_tool_use.len())
}

fn has_cursor_hook(root: &Value) -> Result<bool, String> {
    let Some(pre_tool_use) = root.get("hooks").and_then(|hooks| hooks.get("preToolUse")) else {
        return Ok(false);
    };
    let pre_tool_use = pre_tool_use
        .as_array()
        .ok_or_else(|| "st: Cursor hooks.preToolUse must be an array".to_string())?;
    Ok(pre_tool_use.iter().any(|handler| {
        handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| json_util::is_syntext_hook_command(command, Some("cursor")))
    }))
}
