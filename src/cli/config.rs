//! Config resolution from CLI args and environment.
//!
//! Extracted from `mod.rs` to keep that file under the 400-line quality gate.

use std::path::PathBuf;

use crate::Config;

use super::Cli;

/// Hard ceiling for `SYNTEXT_MAX_FILE_SIZE` (512 MiB).
///
/// Prevents `take(0)` overflow when the env var is set to a very large value.
pub(super) const MAX_FILE_SIZE_CEILING: u64 = 536_870_912;

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

    // --no-update or SYNTEXT_NO_AUTO_UPDATE=1 disables auto-update-on-search.
    let auto_update = !cli.no_update
        && !matches!(
            std::env::var("SYNTEXT_NO_AUTO_UPDATE").ok().as_deref(),
            Some("1") | Some("true")
        );

    // SYNTEXT_NO_ASYNC_UPDATE=1 disables the detached `st update --quiet`
    // catch-up spawned after search results print when the index is known
    // stale beyond the search-time budget. Default: enabled.
    let auto_update_async_catchup =
        resolve_auto_update_async_catchup(std::env::var("SYNTEXT_NO_ASYNC_UPDATE").ok().as_deref());

    Config {
        max_file_size,
        max_segments: 10,
        index_dir,
        repo_root,
        verbose: false,
        strict_permissions: true,
        verify_on_open,
        recalibrate: false,
        auto_update,
        auto_update_max_files: std::env::var("SYNTEXT_AUTO_UPDATE_MAX_FILES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(200),
        auto_update_budget_ms: std::env::var("SYNTEXT_AUTO_UPDATE_BUDGET_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(150),
        auto_update_async_catchup,
        #[cfg(feature = "rayon")]
        thread_pool: None,
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

/// Determine whether the async catch-up update (a detached `st update
/// --quiet` spawned after stale search results print) is enabled, given the
/// raw `SYNTEXT_NO_ASYNC_UPDATE` environment value.
///
/// Extracted so tests can exercise both branches without mutating the
/// process environment via `set_var`/`remove_var`, which is racy under the
/// parallel test harness (see the doc comment on `clamp_max_file_size`).
pub(super) fn resolve_auto_update_async_catchup(raw: Option<&str>) -> bool {
    !matches!(raw, Some("1") | Some("true"))
}

/// Walk up from CWD looking for a `.git` directory.
pub(super) fn detect_repo_root() -> Option<PathBuf> {
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
/// `--index-dir` (or `SYNTEXT_INDEX_DIR` environment variable, which is parsed
/// by Clap and populated into the CLI args) is passed directly to
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let resolved = if index_dir.is_absolute() {
        index_dir.to_path_buf()
    } else {
        cwd.join(index_dir)
    };

    let mut ancestor = resolved.clone();
    let mut popped = Vec::new();
    while !ancestor.exists() {
        if let Some(p) = ancestor.parent() {
            if let Some(file_name) = ancestor.file_name() {
                popped.push(file_name.to_os_string());
            }
            ancestor = p.to_path_buf();
        } else {
            break;
        }
    }
    let mut canonical = std::fs::canonicalize(&ancestor).unwrap_or(ancestor);
    for file_name in popped.into_iter().rev() {
        canonical.push(file_name);
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
    let dir_str = canonical.to_string_lossy();
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let resolved = if index_dir.is_absolute() {
        index_dir.to_path_buf()
    } else {
        cwd.join(index_dir)
    };

    let mut ancestor = resolved.clone();
    let mut popped = Vec::new();
    while !ancestor.exists() {
        if let Some(p) = ancestor.parent() {
            if let Some(file_name) = ancestor.file_name() {
                popped.push(file_name.to_os_string());
            }
            ancestor = p.to_path_buf();
        } else {
            break;
        }
    }
    let mut canonical = std::fs::canonicalize(&ancestor).unwrap_or(ancestor);
    for file_name in popped.into_iter().rev() {
        canonical.push(file_name);
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
    // Normalize the input path: lowercase, forward slashes -> backslashes,
    // and strip \\?\ prefix if present on Windows canonicalized paths.
    let mut dir_lower = canonical
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\");
    if dir_lower.starts_with("\\\\?\\") {
        dir_lower = dir_lower[4..].to_string();
    }
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

#[cfg(test)]
mod tests {
    use super::resolve_config;
    use crate::cli::Cli;
    use clap::Parser;

    #[test]
    fn auto_update_enabled_by_default() {
        let cli = Cli::try_parse_from(["st", "pattern"]).expect("parse failed");
        let config = resolve_config(&cli);
        assert!(config.auto_update, "auto_update should be true by default");
    }

    #[test]
    fn no_update_flag_disables_auto_update() {
        let cli = Cli::try_parse_from(["st", "--no-update", "pattern"]).expect("parse failed");
        let config = resolve_config(&cli);
        assert!(
            !config.auto_update,
            "--no-update should set auto_update=false"
        );
    }

    #[test]
    fn auto_update_defaults_are_sensible() {
        let cli = Cli::try_parse_from(["st", "pattern"]).expect("parse failed");
        let config = resolve_config(&cli);
        assert!(config.auto_update_max_files > 0);
        assert!(config.auto_update_budget_ms > 0);
    }

    #[test]
    fn async_catchup_enabled_by_default() {
        use super::resolve_auto_update_async_catchup;
        assert!(resolve_auto_update_async_catchup(None));
    }

    #[test]
    fn async_catchup_disabled_by_env() {
        use super::resolve_auto_update_async_catchup;
        assert!(!resolve_auto_update_async_catchup(Some("1")));
        assert!(!resolve_auto_update_async_catchup(Some("true")));
    }

    #[test]
    fn async_catchup_enabled_for_unrecognized_values() {
        use super::resolve_auto_update_async_catchup;
        assert!(resolve_auto_update_async_catchup(Some("0")));
        assert!(resolve_auto_update_async_catchup(Some("")));
    }

    #[test]
    fn config_honors_no_async_update_env_absent() {
        // Sanity check: resolve_config wires resolve_auto_update_async_catchup
        // through to Config, defaulting to enabled when the env var is unset
        // in this test process.
        if std::env::var("SYNTEXT_NO_ASYNC_UPDATE").is_err() {
            let cli = Cli::try_parse_from(["st", "pattern"]).expect("parse failed");
            let config = resolve_config(&cli);
            assert!(config.auto_update_async_catchup);
        }
    }
}
