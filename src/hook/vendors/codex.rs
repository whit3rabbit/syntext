//! Codex CLI rules-file integration.

use std::path::{Path, PathBuf};

use crate::hook::core::{files, instructions};

use super::{InstallScope, Outcome};

pub(crate) struct Paths {
    pub(crate) awareness: PathBuf,
    pub(crate) agents_md: PathBuf,
    pub(crate) reference: String,
}

pub(crate) fn install(scope: InstallScope) -> Result<Outcome, String> {
    let paths = paths(scope)?;
    install_at(&paths)
}

pub(crate) fn uninstall(scope: InstallScope) -> Result<Outcome, String> {
    let paths = paths(scope)?;
    uninstall_at(&paths)
}

pub(crate) fn show(scope: InstallScope) -> Result<Outcome, String> {
    let paths = paths(scope)?;
    show_at(&paths)
}

fn paths(scope: InstallScope) -> Result<Paths, String> {
    match scope {
        InstallScope::Global => {
            let root = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or(files::home_dir()?.join(".codex"));
            let awareness = root.join(instructions::AWARENESS_FILE);
            Ok(Paths {
                reference: format!("@{}", awareness.display()),
                awareness,
                agents_md: root.join("AGENTS.md"),
            })
        }
        InstallScope::Project => {
            let cwd = std::env::current_dir()
                .map_err(|err| format!("st: failed to read current directory: {err}"))?;
            let root = files::project_root(&cwd);
            Ok(Paths {
                awareness: root.join(instructions::AWARENESS_FILE),
                agents_md: root.join("AGENTS.md"),
                reference: format!("@{}", instructions::AWARENESS_FILE),
            })
        }
    }
}

pub(crate) fn install_at(paths: &Paths) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    if files::write_text_if_changed(&paths.awareness, instructions::AWARENESS)? {
        outcome.changed.push(paths.awareness.clone());
    }
    if instructions::ensure_line(&paths.agents_md, &paths.reference)? {
        outcome.changed.push(paths.agents_md.clone());
    }
    outcome.installed = true;
    Ok(outcome)
}

pub(crate) fn uninstall_at(paths: &Paths) -> Result<Outcome, String> {
    let mut outcome = Outcome::default();
    if files::remove_file_if_exists(&paths.awareness)? {
        outcome.removed.push(paths.awareness.clone());
    }
    if instructions::remove_line(&paths.agents_md, &paths.reference)? {
        outcome.changed.push(paths.agents_md.clone());
    }
    Ok(outcome)
}

pub(crate) fn show_at(paths: &Paths) -> Result<Outcome, String> {
    Ok(Outcome {
        installed: paths.awareness.exists() && contains_line(&paths.agents_md, &paths.reference)?,
        ..Outcome::default()
    })
}

fn contains_line(path: &Path, line: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    Ok(text.lines().any(|existing| existing.trim() == line))
}
