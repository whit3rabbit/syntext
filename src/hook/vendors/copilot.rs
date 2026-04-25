//! GitHub Copilot integration installer.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::hook::core::{files, instructions, json as json_util};

use super::Outcome;

pub(crate) fn install(st_program: &str) -> Result<Outcome, String> {
    let paths = paths()?;
    install_at(&paths, st_program)
}

pub(crate) fn uninstall() -> Result<Outcome, String> {
    let paths = paths()?;
    uninstall_at(&paths)
}

pub(crate) fn show() -> Result<Outcome, String> {
    let paths = paths()?;
    show_at(&paths)
}

pub(crate) struct Paths {
    pub(crate) hook_config: PathBuf,
    pub(crate) instructions: PathBuf,
}

fn paths() -> Result<Paths, String> {
    let cwd = std::env::current_dir()
        .map_err(|err| format!("st: failed to read current directory: {err}"))?;
    let root = files::project_root(&cwd);
    Ok(Paths {
        hook_config: root
            .join(".github")
            .join("hooks")
            .join("syntext-rewrite.json"),
        instructions: root.join(".github").join("copilot-instructions.md"),
    })
}

pub(crate) fn install_at(paths: &Paths, st_program: &str) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    let mut config = json_util::read_json_object(&paths.hook_config)?;
    let before = config.clone();
    remove_syntext_hooks(&mut config)?;
    ensure_copilot_hook(&mut config, &json_util::hook_command(st_program, "copilot"))?;
    if config != before && json_util::write_json_if_changed(&paths.hook_config, &config)? {
        outcome.changed.push(paths.hook_config.clone());
    }

    let block = instructions::syntext_block("copilot", "Syntext Code Search");
    if instructions::ensure_block(&paths.instructions, "copilot", &block)? {
        outcome.changed.push(paths.instructions.clone());
    }
    outcome.installed = true;
    Ok(outcome)
}

pub(crate) fn uninstall_at(paths: &Paths) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    if paths.hook_config.exists() {
        let mut config = json_util::read_json_object(&paths.hook_config)?;
        let removed = remove_syntext_hooks(&mut config)?;
        if removed > 0 && json_util::write_json_if_changed(&paths.hook_config, &config)? {
            outcome.changed.push(paths.hook_config.clone());
        }
    }
    if instructions::remove_block(&paths.instructions, "copilot")? {
        outcome.changed.push(paths.instructions.clone());
    }
    Ok(outcome)
}

pub(crate) fn show_at(paths: &Paths) -> Result<Outcome, String> {
    let config = json_util::read_json_object(&paths.hook_config)?;
    Ok(Outcome {
        installed: has_copilot_hook(&config)? && contains_marker(&paths.instructions, "copilot")?,
        ..Outcome::default()
    })
}

fn ensure_copilot_hook(root: &mut Value, command: &str) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "st: Copilot hook config root must be an object".to_string())?;
    let hooks = root_obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "st: Copilot hooks must be an object".to_string())?;
    let pre_tool_use = hooks.entry("PreToolUse").or_insert_with(|| json!([]));
    let pre_tool_use = pre_tool_use
        .as_array_mut()
        .ok_or_else(|| "st: Copilot hooks.PreToolUse must be an array".to_string())?;
    let handler = json!({
        "type": "command",
        "command": command,
        "cwd": ".",
        "timeout": 5
    });
    if !pre_tool_use.contains(&handler) {
        pre_tool_use.push(handler);
    }
    Ok(())
}

fn remove_syntext_hooks(root: &mut Value) -> Result<usize, String> {
    let Some(pre_tool_use) = root
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut("PreToolUse"))
    else {
        return Ok(0);
    };
    let pre_tool_use = pre_tool_use
        .as_array_mut()
        .ok_or_else(|| "st: Copilot hooks.PreToolUse must be an array".to_string())?;
    let before = pre_tool_use.len();
    pre_tool_use.retain(|handler| {
        !handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| json_util::is_syntext_hook_command(command, Some("copilot")))
    });
    Ok(before - pre_tool_use.len())
}

fn has_copilot_hook(root: &Value) -> Result<bool, String> {
    let Some(pre_tool_use) = root.get("hooks").and_then(|hooks| hooks.get("PreToolUse")) else {
        return Ok(false);
    };
    let pre_tool_use = pre_tool_use
        .as_array()
        .ok_or_else(|| "st: Copilot hooks.PreToolUse must be an array".to_string())?;
    Ok(pre_tool_use.iter().any(|handler| {
        handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| json_util::is_syntext_hook_command(command, Some("copilot")))
    }))
}

fn contains_marker(path: &Path, id: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    Ok(text.contains(&instructions::marker_start(id)))
}
