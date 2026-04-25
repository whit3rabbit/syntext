//! Gemini CLI integration installer.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::hook::core::{files, instructions, json as json_util, shell};

use super::Outcome;

pub(crate) struct Paths {
    pub(crate) settings: PathBuf,
    pub(crate) hook_script: PathBuf,
    pub(crate) gemini_md: PathBuf,
}

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

fn paths() -> Result<Paths, String> {
    let gemini = files::home_dir()?.join(".gemini");
    Ok(Paths {
        settings: gemini.join("settings.json"),
        hook_script: gemini.join("hooks").join("syntext-hook.sh"),
        gemini_md: gemini.join("GEMINI.md"),
    })
}

pub(crate) fn install_at(paths: &Paths, st_program: &str) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    let script = format!(
        "#!/bin/sh\nexec {} __hook gemini\n",
        shell::shell_quote(st_program)
    );
    if files::write_text_if_changed(&paths.hook_script, &script)? {
        files::set_executable(&paths.hook_script)?;
        outcome.changed.push(paths.hook_script.clone());
    }

    let command = shell::shell_quote(&paths.hook_script.to_string_lossy());
    let mut settings = json_util::read_json_object(&paths.settings)?;
    let before = settings.clone();
    remove_syntext_hook(&mut settings, &command)?;
    add_gemini_hook(&mut settings, &command)?;
    if settings != before && json_util::write_json_if_changed(&paths.settings, &settings)? {
        outcome.changed.push(paths.settings.clone());
    }

    if instructions::ensure_block(
        &paths.gemini_md,
        "gemini",
        &instructions::syntext_block("gemini", "Syntext Code Search"),
    )? {
        outcome.changed.push(paths.gemini_md.clone());
    }
    outcome.installed = true;
    Ok(outcome)
}

pub(crate) fn uninstall_at(paths: &Paths) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    let command = shell::shell_quote(&paths.hook_script.to_string_lossy());
    if paths.settings.exists() {
        let mut settings = json_util::read_json_object(&paths.settings)?;
        let removed = remove_syntext_hook(&mut settings, &command)?;
        if removed > 0 && json_util::write_json_if_changed(&paths.settings, &settings)? {
            outcome.changed.push(paths.settings.clone());
        }
    }
    if files::remove_file_if_exists(&paths.hook_script)? {
        outcome.removed.push(paths.hook_script.clone());
    }
    if instructions::remove_block(&paths.gemini_md, "gemini")? {
        outcome.changed.push(paths.gemini_md.clone());
    }
    Ok(outcome)
}

pub(crate) fn show_at(paths: &Paths) -> Result<Outcome, String> {
    let command = shell::shell_quote(&paths.hook_script.to_string_lossy());
    let settings = json_util::read_json_object(&paths.settings)?;
    Ok(Outcome {
        installed: paths.hook_script.exists() && has_gemini_hook(&settings, &command)?,
        ..Outcome::default()
    })
}

fn add_gemini_hook(root: &mut Value, command: &str) -> Result<(), String> {
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| "st: Gemini settings root must be an object".to_string())?;
    let hooks = root_obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "st: Gemini settings.hooks must be an object".to_string())?;
    let before_tool = hooks.entry("BeforeTool").or_insert_with(|| json!([]));
    let before_tool = before_tool
        .as_array_mut()
        .ok_or_else(|| "st: Gemini settings.hooks.BeforeTool must be an array".to_string())?;
    let handler = json!({ "type": "command", "command": command });
    if let Some(group) = before_tool
        .iter_mut()
        .find(|group| group.get("matcher").and_then(Value::as_str) == Some("run_shell_command"))
    {
        let group_obj = group
            .as_object_mut()
            .ok_or_else(|| "st: Gemini hook matcher group must be an object".to_string())?;
        let hooks = group_obj.entry("hooks").or_insert_with(|| json!([]));
        let hooks = hooks
            .as_array_mut()
            .ok_or_else(|| "st: Gemini hook group hooks must be an array".to_string())?;
        if !hooks.contains(&handler) {
            hooks.push(handler);
        }
        return Ok(());
    }
    before_tool.push(json!({ "matcher": "run_shell_command", "hooks": [handler] }));
    Ok(())
}

fn remove_syntext_hook(root: &mut Value, command: &str) -> Result<usize, String> {
    let Some(before_tool) = root
        .get_mut("hooks")
        .and_then(|hooks| hooks.get_mut("BeforeTool"))
    else {
        return Ok(0);
    };
    let before_tool = before_tool
        .as_array_mut()
        .ok_or_else(|| "st: Gemini settings.hooks.BeforeTool must be an array".to_string())?;
    let mut removed = 0;
    let mut empty_groups = Vec::new();
    for (index, group) in before_tool.iter_mut().enumerate() {
        let Some(hooks) = group.get_mut("hooks") else {
            continue;
        };
        let hooks = hooks
            .as_array_mut()
            .ok_or_else(|| "st: Gemini hook group hooks must be an array".to_string())?;
        let before = hooks.len();
        hooks.retain(|handler| handler.get("command").and_then(Value::as_str) != Some(command));
        removed += before - hooks.len();
        if before != hooks.len() && hooks.is_empty() {
            empty_groups.push(index);
        }
    }
    for index in empty_groups.into_iter().rev() {
        before_tool.remove(index);
    }
    Ok(removed)
}

fn has_gemini_hook(root: &Value, command: &str) -> Result<bool, String> {
    let Some(before_tool) = root.get("hooks").and_then(|hooks| hooks.get("BeforeTool")) else {
        return Ok(false);
    };
    let before_tool = before_tool
        .as_array()
        .ok_or_else(|| "st: Gemini settings.hooks.BeforeTool must be an array".to_string())?;
    Ok(before_tool.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks
                    .iter()
                    .any(|handler| handler.get("command").and_then(Value::as_str) == Some(command))
            })
    }))
}
