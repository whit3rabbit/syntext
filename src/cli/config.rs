//! Config resolution from CLI args and environment.
//!
//! Extracted from `mod.rs` to keep that file under the 400-line quality gate.

use std::path::PathBuf;

use crate::Config;

use super::Cli;

/// Hard ceiling for `SYNTEXT_MAX_FILE_SIZE` (1 GiB).
///
/// Prevents `take(0)` overflow when the env var is set to a very large value.
pub(super) const MAX_FILE_SIZE_CEILING: u64 = 1_073_741_824;

/// Resolve Config from CLI args and environment.
pub(super) fn resolve_config(cli: &Cli) -> Config {
    let repo_root = cli
        .repo_root
        .clone()
        .or_else(detect_repo_root)
        .unwrap_or_else(|| PathBuf::from("."));

    let index_dir = {
        let raw = cli
            .index_dir
            .clone()
            .unwrap_or_else(|| repo_root.join(".syntext"));
        // Security: reject paths that overlap system directories before any
        // fs::create_dir_all or fs::set_permissions call in build_index.
        if let Err(msg) = validate_index_dir(&raw) {
            eprintln!("{msg}");
            std::process::exit(2);
        }
        raw
    };

    let max_file_size = parse_max_file_size();

    // SYNTEXT_VERIFY_ON_OPEN=1 restores the full .post checksum pass on every
    // Index::open (paranoid mode); see Config::verify_on_open.
    let verify_on_open = matches!(
        std::env::var("SYNTEXT_VERIFY_ON_OPEN").ok().as_deref(),
        Some("1") | Some("true")
    );

    Config {
        max_file_size,
        max_segments: 10,
        index_dir,
        repo_root,
        verbose: false,
        strict_permissions: true,
        verify_on_open,
        recalibrate: false,
    }
}

/// Read and clamp `SYNTEXT_MAX_FILE_SIZE` from the environment.
///
/// Returns the configured value clamped to [`MAX_FILE_SIZE_CEILING`], or
/// the 10 MiB default when the variable is absent or unparseable.
fn parse_max_file_size() -> u64 {
    clamp_max_file_size(
        std::env::var("SYNTEXT_MAX_FILE_SIZE")
            .ok()
            .and_then(|v| v.parse::<u64>().ok()),
    )
}

/// Apply the 10 MiB default and the 1 GiB ceiling to an optional raw value.
///
/// Extracted from `parse_max_file_size` so tests can exercise the clamping
/// logic directly without mutating the process environment via `set_var`.
/// `std::env::set_var` is not thread-safe: Rust's test harness runs tests in
/// parallel, and any concurrent test reading `SYNTEXT_MAX_FILE_SIZE` during a
/// `set_var` / `remove_var` pair would observe the injected value, causing
/// non-deterministic test behaviour.
pub(super) fn clamp_max_file_size(raw: Option<u64>) -> u64 {
    raw.unwrap_or(10 * 1024 * 1024).min(MAX_FILE_SIZE_CEILING)
}

/// Walk up from CWD looking for a `.git` directory.
fn detect_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Reject `index_dir` values that overlap known-sensitive system path prefixes.
///
/// `--index-dir` / `SYNTEXT_INDEX_DIR` is passed directly to
/// `fs::create_dir_all` + `fs::set_permissions(0o700)` in `build_index`.
/// If the value is sourced from untrusted input (e.g., a CI artifact field)
/// and `st index` runs as root, a value like `/etc` would `chmod 0700 /etc`,
/// making it inaccessible to non-root processes and breaking the system.
///
/// Only absolute paths matching a known-sensitive prefix are rejected.
/// Relative paths and paths under user-owned directories are always accepted.
///
/// The core prefix-matching logic is in `overlaps_sensitive_prefix` so it can
/// be unit-tested on any platform.
#[cfg(unix)]
fn validate_index_dir(index_dir: &std::path::Path) -> Result<(), String> {
    if !index_dir.is_absolute() {
        return Ok(());
    }
    const SENSITIVE_PREFIXES: &[&str] = &[
        "/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/sys",
        "/proc",
        "/dev",
        "/boot",
        "/root",
        // macOS: /etc and /var are symlinks into /private; check both.
        "/System",
        "/Library",
        "/private/etc",
        "/private/var/root",
    ];
    let dir_str = index_dir.to_string_lossy();
    if let Some(matched) = overlaps_sensitive_prefix(&dir_str, SENSITIVE_PREFIXES, '/') {
        return Err(format!(
            "st: refusing --index-dir '{}': overlaps system path '{matched}'; \
             use a path under the repository or a user-owned directory",
            index_dir.display(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_index_dir(index_dir: &std::path::Path) -> Result<(), String> {
    if !index_dir.is_absolute() {
        return Ok(());
    }
    // Build sensitive prefixes from environment variables (handles non-standard
    // Windows installs, e.g. Windows on D:\). Fall back to hardcoded defaults.
    let mut sensitive: Vec<String> = Vec::new();
    for var in [
        "SYSTEMROOT",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMDATA",
    ] {
        if let Some(val) = std::env::var_os(var) {
            let lower = val.to_string_lossy().to_lowercase().replace('/', "\\");
            if !sensitive.contains(&lower) {
                sensitive.push(lower);
            }
        }
    }
    for fb in [
        "c:\\windows",
        "c:\\program files",
        "c:\\program files (x86)",
        "c:\\programdata",
    ] {
        let s = fb.to_string();
        if !sensitive.contains(&s) {
            sensitive.push(s);
        }
    }
    // Normalize the input path: lowercase, forward slashes -> backslashes.
    let dir_lower = index_dir
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\");
    let prefixes: Vec<&str> = sensitive.iter().map(|s| s.as_str()).collect();
    if let Some(matched) = overlaps_sensitive_prefix(&dir_lower, &prefixes, '\\') {
        return Err(format!(
            "st: refusing --index-dir '{}': overlaps system path '{matched}'; \
             use a path under the repository or a user-owned directory",
            index_dir.display(),
        ));
    }
    Ok(())
}

/// Return the matched prefix if `dir` equals or is a child of any entry in
/// `prefixes` (separated by `sep`). Platform-independent so it can be tested
/// on any OS.
pub(super) fn overlaps_sensitive_prefix<'a>(
    dir: &str,
    prefixes: &[&'a str],
    sep: char,
) -> Option<&'a str> {
    for &prefix in prefixes {
        if dir == prefix || dir.starts_with(&format!("{prefix}{sep}")) {
            return Some(prefix);
        }
    }
    None
}
