//! Claude Code integration installer.

use std::path::{Path, PathBuf};

use crate::hook::core::{files, instructions, json};

use super::{InstallScope, Outcome};

pub(crate) struct Paths {
    pub(crate) settings: Option<PathBuf>,
    pub(crate) claude_md: PathBuf,
    pub(crate) awareness: Option<PathBuf>,
}

pub(crate) fn install(scope: InstallScope, st_program: &str) -> Result<Outcome, String> {
    let paths = resolve_paths(scope)?;
    install_at(&paths, st_program)
}

pub(crate) fn uninstall(scope: InstallScope) -> Result<Outcome, String> {
    let paths = resolve_paths(scope)?;
    uninstall_at(&paths)
}

pub(crate) fn show(scope: InstallScope) -> Result<Outcome, String> {
    let paths = resolve_paths(scope)?;
    show_at(&paths)
}

fn resolve_paths(scope: InstallScope) -> Result<Paths, String> {
    match scope {
        InstallScope::Global => {
            let claude_dir = files::home_dir()?.join(".claude");
            Ok(Paths {
                settings: Some(claude_dir.join("settings.json")),
                claude_md: claude_dir.join("CLAUDE.md"),
                awareness: Some(claude_dir.join(instructions::AWARENESS_FILE)),
            })
        }
        InstallScope::Project => {
            let cwd = std::env::current_dir()
                .map_err(|err| format!("st: failed to read current directory: {err}"))?;
            let root = files::project_root(&cwd);
            Ok(Paths {
                settings: None,
                claude_md: root.join("CLAUDE.md"),
                awareness: None,
            })
        }
    }
}

pub(crate) fn install_at(paths: &Paths, st_program: &str) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();

    if let Some(settings_path) = &paths.settings {
        let mut settings = json::read_json_object(settings_path)?;
        let before = settings.clone();
        json::remove_grouped_hooks(&mut settings, "PreToolUse")?;
        json::add_grouped_hook(
            &mut settings,
            "PreToolUse",
            "Bash",
            &json::hook_command(st_program, "claude"),
        )?;
        json::add_grouped_hook(
            &mut settings,
            "PreToolUse",
            "Grep",
            &json::hook_command(st_program, "claude-grep-block"),
        )?;
        if settings != before && json::write_json_if_changed(settings_path, &settings)? {
            outcome.changed.push(settings_path.clone());
        }
    }

    match &paths.awareness {
        Some(awareness) => {
            if files::write_text_if_changed(awareness, instructions::AWARENESS)? {
                outcome.changed.push(awareness.clone());
            }
            if instructions::ensure_line(&paths.claude_md, instructions::AWARENESS_REF)? {
                outcome.changed.push(paths.claude_md.clone());
            }
        }
        None => {
            let block = instructions::syntext_block("claude", "Code Search");
            if instructions::ensure_block(&paths.claude_md, "claude", &block)? {
                outcome.changed.push(paths.claude_md.clone());
            }
        }
    }

    outcome.installed = true;
    Ok(outcome)
}

pub(crate) fn uninstall_at(paths: &Paths) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();

    if let Some(settings_path) = &paths.settings {
        if settings_path.exists() {
            let mut settings = json::read_json_object(settings_path)?;
            let removed = json::remove_grouped_hooks(&mut settings, "PreToolUse")?;
            if removed > 0 && json::write_json_if_changed(settings_path, &settings)? {
                outcome.changed.push(settings_path.clone());
            }
        }
    }

    match &paths.awareness {
        Some(awareness) => {
            if files::remove_file_if_exists(awareness)? {
                outcome.removed.push(awareness.clone());
            }
            if instructions::remove_line(&paths.claude_md, instructions::AWARENESS_REF)? {
                outcome.changed.push(paths.claude_md.clone());
            }
        }
        None => {
            if instructions::remove_block(&paths.claude_md, "claude")? {
                outcome.changed.push(paths.claude_md.clone());
            }
        }
    }

    outcome.installed = false;
    Ok(outcome)
}

pub(crate) fn show_at(paths: &Paths) -> Result<Outcome, String> {
    let installed = if let Some(settings_path) = &paths.settings {
        let settings = json::read_json_object(settings_path)?;
        json::has_grouped_hook(&settings, "PreToolUse", "claude")?
            && json::has_grouped_hook(&settings, "PreToolUse", "claude-grep-block")?
    } else {
        contains_marker(&paths.claude_md, "claude")?
    };
    Ok(Outcome {
        installed,
        ..Outcome::default()
    })
}

fn contains_marker(path: &Path, id: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    Ok(text.contains(&instructions::marker_start(id)))
}
