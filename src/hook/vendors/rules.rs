//! Rules-only workspace integration helpers.

use std::path::Path;

use crate::hook::core::{files, instructions};

use super::Outcome;

pub(crate) fn install(relative_path: &str, id: &str, heading: &str) -> Result<Outcome, String> {
    let path = project_path(relative_path)?;
    install_at(&path, id, heading)
}

pub(crate) fn install_at(path: &Path, id: &str, heading: &str) -> Result<Outcome, String> {
    let block = instructions::syntext_block(id, heading);
    let changed = instructions::ensure_block(path, id, &block)?;
    Ok(Outcome {
        installed: true,
        changed: changed.then(|| path.to_path_buf()).into_iter().collect(),
        removed: Vec::new(),
    })
}

pub(crate) fn uninstall(relative_path: &str, id: &str) -> Result<Outcome, String> {
    let path = project_path(relative_path)?;
    uninstall_at(&path, id)
}

pub(crate) fn uninstall_at(path: &Path, id: &str) -> Result<Outcome, String> {
    let changed = instructions::remove_block(path, id)?;
    Ok(Outcome {
        installed: false,
        changed: changed.then(|| path.to_path_buf()).into_iter().collect(),
        removed: Vec::new(),
    })
}

pub(crate) fn show(relative_path: &str, id: &str) -> Result<Outcome, String> {
    let path = project_path(relative_path)?;
    show_at(&path, id)
}

pub(crate) fn show_at(path: &Path, id: &str) -> Result<Outcome, String> {
    Ok(Outcome {
        installed: contains_marker(path, id)?,
        ..Outcome::default()
    })
}

fn project_path(relative_path: &str) -> Result<std::path::PathBuf, String> {
    let cwd = std::env::current_dir()
        .map_err(|err| format!("st: failed to read current directory: {err}"))?;
    Ok(files::project_root(&cwd).join(relative_path))
}

fn contains_marker(path: &Path, id: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    Ok(text.contains(&instructions::marker_start(id)))
}
