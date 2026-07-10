//! `core.fsmonitor` UX helpers: the one-time tip printed when git change
//! detection is slow, and the opt-in enable path used by `st init --fsmonitor`.
//!
//! Split from `freshness.rs` to keep that file under the 400-line quality gate.
//! Re-exported from `freshness` so existing `freshness::enable_fsmonitor` /
//! `freshness::maybe_print_fsmonitor_tip` call sites (and the `freshness_tests`
//! module, via `use super::*`) resolve unchanged.

use std::path::Path;
use std::process::{Command, Stdio};

/// Name of the stamp file (relative to the index dir) written the first time
/// the `core.fsmonitor` tip is printed, so it prints at most once per index.
pub(crate) const FSMONITOR_TIP_STAMP: &str = "fsmonitor-tip-shown";

/// Print a one-time tip suggesting `git config core.fsmonitor true` when
/// git change detection is taking a large share of the auto-update time
/// budget, and detection could be made near-instant by fsmonitor.
///
/// Fires only when all of the following hold:
/// - `detect_elapsed_ms` is more than half of `budget_ms` (a zero budget
///   never fires: there is no meaningful "half" of zero).
/// - the stamp file (`<index_dir>/fsmonitor-tip-shown`) does not already
///   exist -- this makes the tip print at most once per index.
/// - `core.fsmonitor` is not already set to `true` in `repo_root`.
///
/// This function only ever reads git config and writes the stamp file; it
/// never sets `core.fsmonitor` itself (see `cmd_init`'s `--fsmonitor` flag
/// for the only place that does, since enabling fsmonitor starts a
/// background daemon and must be explicit, opt-in consent).
///
/// Errors probing `core.fsmonitor` or writing the stamp file are swallowed:
/// this is a best-effort UX hint, never something that should affect the
/// search/update outcome or its exit code.
pub fn maybe_print_fsmonitor_tip(
    repo_root: &Path,
    git: &Path,
    index_dir: &Path,
    detect_elapsed_ms: u64,
    budget_ms: u64,
) {
    if budget_ms == 0 || detect_elapsed_ms * 2 < budget_ms {
        return;
    }
    let stamp = index_dir.join(FSMONITOR_TIP_STAMP);
    if stamp.exists() {
        return;
    }
    if is_fsmonitor_enabled(repo_root, git) {
        return;
    }
    eprintln!(
        "st: tip \u{2014} 'git config core.fsmonitor true' makes freshness checks near-instant on this repo"
    );
    let _ = std::fs::write(&stamp, b"");
}

/// Returns `true` when `git config core.fsmonitor` resolves to `true` in
/// `repo_root`. Any error (no git, non-git dir, unset config) is treated as
/// "not enabled" so the tip can still be offered.
pub(crate) fn is_fsmonitor_enabled(repo_root: &Path, git: &Path) -> bool {
    let output = Command::new(git)
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "core.fsmonitor"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(out) => out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true",
        Err(_) => false,
    }
}

/// Set `core.fsmonitor = true` in `repo_root` via `git config`.
///
/// This is the only place in `syntext` that enables fsmonitor: enabling it
/// starts a background watchman-style daemon, so it must only ever happen
/// from explicit, opt-in user consent (`st init --fsmonitor`), never as a
/// side effect of the bounded auto-update tip (see `maybe_print_fsmonitor_tip`,
/// which only reads config and never writes it).
///
/// Returns `true` when `git config` exits successfully, `false` on any
/// error (no git, non-git directory, or a failed git invocation).
pub fn enable_fsmonitor(repo_root: &Path, git: &Path) -> bool {
    Command::new(git)
        .arg("-C")
        .arg(repo_root)
        .args(["config", "core.fsmonitor", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
