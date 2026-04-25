//! Shared filesystem helpers for hook installers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .ok_or_else(|| "st: cannot determine home directory".to_string())
}

pub(crate) fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.to_path_buf())
}

pub(crate) fn current_st_program() -> Result<String, String> {
    let path = std::env::current_exe()
        .map_err(|err| format!("st: failed to resolve current executable: {err}"))?;
    path.into_os_string()
        .into_string()
        .map_err(|_| "st: current executable path is not valid UTF-8".to_string())
}

pub(crate) fn write_text_if_changed(path: &Path, content: &str) -> Result<bool, String> {
    if path.exists() {
        let existing = fs::read_to_string(path)
            .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
        if existing == content {
            return Ok(false);
        }
        backup_existing(path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("st: failed to create {}: {err}", parent.display()))?;
    }
    write_atomic(path, content)
        .map_err(|err| format!("st: failed to write {}: {err}", path.display()))?;
    Ok(true)
}

pub(crate) fn remove_file_if_exists(path: &Path) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    backup_existing(path)?;
    fs::remove_file(path)
        .map_err(|err| format!("st: failed to remove {}: {err}", path.display()))?;
    Ok(true)
}

pub(crate) fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tmp");
    let tmp = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));
    fs::write(&tmp, content)?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(tmp, path)
}

pub(crate) fn backup_existing(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Ok(backup_path(path));
    }
    let backup = backup_path(path);
    fs::copy(path, &backup).map_err(|err| {
        format!(
            "st: failed to back up {} to {}: {err}",
            path.display(),
            backup.display()
        )
    })?;
    Ok(backup)
}

pub(crate) fn backup_path(path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.with_file_name(format!("{file_name}.bak.{timestamp}.{pid}"))
}

#[cfg(unix)]
pub(crate) fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = fs::metadata(path)
        .map_err(|err| format!("st: failed to stat {}: {err}", path.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    fs::set_permissions(path, perms)
        .map_err(|err| format!("st: failed to chmod {}: {err}", path.display()))
}

#[cfg(not(unix))]
pub(crate) fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}
