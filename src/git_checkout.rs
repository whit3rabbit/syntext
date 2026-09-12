//! Git checkout identity: is this directory a plain clone, a linked worktree,
//! or a submodule, and what is it sitting on?
//!
//! Split from [`crate::git_util`] (git binary resolution and path safety) so
//! that file stays focused and both stay under the 400-line quality gate.
//!
//! Everything here is **plain file reads**: `<root>/.git` and `<gitdir>/HEAD`.
//! No `git` subprocess is spawned. That is load-bearing, not incidental --
//! `index::freshness` budgets exactly one `git status` spawn per change
//! detection, and two tests in `tests/integration/cli.rs` count shim
//! invocations and assert that number. Adding a `git rev-parse --abbrev-ref`
//! here would break both the budget and those tests.
//!
//! # Trust model
//!
//! `.git` and `HEAD` come from whatever repository the user cloned, so their
//! contents are attacker-influenced. Reads are opened `O_NOFOLLOW` and capped
//! at [`MAX_GIT_META_BYTES`], and every string that can reach a terminal goes
//! through [`sanitize_label`] first. The resolved gitdir is deliberately *not*
//! required to live under the repo root: a linked worktree's gitdir legitimately
//! points into the main repository's `.git/worktrees/<name>`. Containment is not
//! the control here; read-only access to two known filenames is.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Cap on `.git` / `HEAD` reads. Both files are a single short line in
/// practice; the cap bounds a hostile repo's ability to make us allocate.
const MAX_GIT_META_BYTES: u64 = 4096;

/// Cap on a displayed worktree name or branch, in characters.
const MAX_LABEL_CHARS: usize = 256;

/// What kind of git checkout a directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutKind {
    /// A plain checkout: `.git` is a directory.
    Main,
    /// A linked worktree (`git worktree add`): `.git` is a file whose gitdir
    /// resolves under `<main>/.git/worktrees/<name>`.
    Linked,
    /// A submodule: `.git` is a file whose gitdir resolves under
    /// `<super>/.git/modules/<name>`.
    Submodule,
    /// A `.git` that exists but does not look like either of the above (an
    /// independent clone nested inside another repo, or an unreadable or
    /// malformed pointer file).
    NestedClone,
}

impl CheckoutKind {
    /// Stable machine-readable name, used for `st status --json`.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckoutKind::Main => "main",
            CheckoutKind::Linked => "linked",
            CheckoutKind::Submodule => "submodule",
            CheckoutKind::NestedClone => "nested-clone",
        }
    }

    /// Noun for the "skipping nested <noun>" build notice.
    pub fn noun(self) -> &'static str {
        match self {
            CheckoutKind::Linked => "worktree",
            CheckoutKind::Submodule => "submodule",
            CheckoutKind::Main | CheckoutKind::NestedClone => "checkout",
        }
    }
}

/// Which checkout a directory is, and what it currently points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutIdentity {
    /// Plain clone, linked worktree, or submodule.
    pub kind: CheckoutKind,
    /// Linked-worktree or submodule name, taken from the gitdir path. `None`
    /// for a plain checkout, or when the pointer file was unreadable.
    pub name: Option<String>,
    /// Current branch, with `refs/heads/` stripped. `None` when HEAD is
    /// detached or unreadable.
    pub branch: Option<String>,
    /// Commit id, set only when HEAD is detached.
    pub head: Option<String>,
}

/// True if `dir` holds its own `.git`, i.e. it is a checkout in its own right.
///
/// Uses `exists()` (which follows symlinks) rather than classifying, so a
/// `.git` this module cannot parse still counts. The index walk skips these
/// subtrees, and it should fail closed: a `.git` it does not understand is
/// still not content belonging to the outer repository.
pub fn is_checkout_root(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Best-effort classification of a directory that [`is_checkout_root`] already
/// accepted. Only ever used to word a message, so it degrades to
/// [`CheckoutKind::NestedClone`] rather than failing.
pub fn classify_nested(dir: &Path) -> CheckoutKind {
    match resolve_gitdir(dir) {
        // A nested `.git` *directory* is an independent clone, never `Main`:
        // `Main` describes the root being indexed, not a subtree of it.
        Some(GitDir {
            via_pointer: false, ..
        })
        | None => CheckoutKind::NestedClone,
        Some(GitDir {
            path,
            via_pointer: true,
        }) => kind_and_name(&path).0,
    }
}

/// Identify the checkout rooted at `root`, including its branch.
///
/// Returns `None` when `root` has no `.git` at all (not a checkout).
pub fn identify_checkout(root: &Path) -> Option<CheckoutIdentity> {
    let gitdir = resolve_gitdir(root)?;
    let (kind, name) = if gitdir.via_pointer {
        kind_and_name(&gitdir.path)
    } else {
        (CheckoutKind::Main, None)
    };
    let (branch, head) = read_head(&gitdir.path);
    Some(CheckoutIdentity {
        kind,
        name,
        branch,
        head,
    })
}

/// A resolved gitdir plus how we got there.
struct GitDir {
    path: PathBuf,
    /// True when `.git` was a pointer *file* (`gitdir: ...`), false when it was
    /// a real directory.
    via_pointer: bool,
}

/// Resolve `<root>/.git` to the directory holding `HEAD`.
fn resolve_gitdir(root: &Path) -> Option<GitDir> {
    let dot_git = root.join(".git");
    // symlink_metadata, not metadata: a `.git` symlink is not something we
    // follow to read files from. `is_checkout_root` still counts it, so such a
    // directory is skipped by the walk and merely classified `NestedClone`.
    let meta = std::fs::symlink_metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(GitDir {
            path: dot_git,
            via_pointer: false,
        });
    }
    if !meta.is_file() {
        return None;
    }
    let text = read_bounded(&dot_git)?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    if target.is_empty() {
        return None;
    }
    let raw = PathBuf::from(target);
    // A relative gitdir is relative to the worktree root. Deliberately not
    // canonicalized: joining is enough to open `HEAD`, and canonicalize would
    // add syscalls and follow symlinks for no benefit.
    let path = if raw.is_absolute() {
        raw
    } else {
        root.join(raw)
    };
    Some(GitDir {
        path,
        via_pointer: true,
    })
}

/// Derive kind and name from a pointer-resolved gitdir path.
///
/// Scans components from the right so that a repository which merely *lives*
/// under a directory called `modules` is not misread as a submodule; the
/// rightmost marker is the one git itself wrote.
fn kind_and_name(gitdir: &Path) -> (CheckoutKind, Option<String>) {
    let comps: Vec<&OsStr> = gitdir
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    for (i, comp) in comps.iter().enumerate().rev() {
        let kind = if *comp == OsStr::new("worktrees") {
            CheckoutKind::Linked
        } else if *comp == OsStr::new("modules") {
            CheckoutKind::Submodule
        } else {
            continue;
        };
        if i + 1 >= comps.len() {
            continue;
        }
        // Submodule names can nest (`.git/modules/a/modules/b`); join whatever
        // follows the marker with `/` so the name round-trips on Windows too.
        let name = comps[i + 1..]
            .iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        return (kind, sanitize_label(&name));
    }
    (CheckoutKind::NestedClone, None)
}

/// Read `<gitdir>/HEAD` into `(branch, detached_head)`. At most one is `Some`.
fn read_head(gitdir: &Path) -> (Option<String>, Option<String>) {
    let Some(text) = read_bounded(&gitdir.join("HEAD")) else {
        return (None, None);
    };
    let first = text.lines().next().unwrap_or_default().trim();
    if let Some(reference) = first.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        (sanitize_label(branch), None)
    } else if crate::git_util::is_hex_commit(first) {
        (None, sanitize_label(first))
    } else {
        // Neither a symref nor an object id: corrupt or not a HEAD at all.
        (None, None)
    }
}

/// Read at most [`MAX_GIT_META_BYTES`] from `path`, without following a
/// symlink on the final component. `None` on any failure.
fn read_bounded(path: &Path) -> Option<String> {
    let file = crate::index::io_util::open_readonly_nofollow(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_GIT_META_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Accept a label only if it is safe to print to a terminal.
///
/// Rejects (rather than strips) control characters: git forbids them in ref
/// names, so their presence means the file is corrupt or hostile, and a
/// half-scrubbed name is worse than no name. Also bounds the length, so a
/// 4 KiB "branch" cannot blow up `st status` output.
fn sanitize_label(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_LABEL_CHARS {
        return None;
    }
    if trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
#[path = "git_checkout_tests.rs"]
mod tests;
