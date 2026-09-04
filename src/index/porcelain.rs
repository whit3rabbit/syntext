//! Parser for `git status --porcelain=v1 -z` output.
//!
//! `detect_changed_files` (`freshness.rs`) runs one `git status` per search
//! and needs only the path out of each record. Porcelain v1 with `-z` is the
//! one status format whose record grammar is stable and unquoted:
//!
//! ```text
//! XY<SP><path>NUL
//! ```
//!
//! `XY` is two status bytes, byte 2 is always a single space, and the path
//! runs to the NUL. Nothing is quoted or escaped (`core.quotePath` does not
//! apply), so a path may legally start with a space or contain a newline.
//! Rename and copy records (`R`/`C` in either column) carry a second
//! NUL-terminated path, the original. `detect_changed_files` passes
//! `--no-renames`, so those never appear, but the parser consumes the second
//! path anyway so a config surprise can never misalign the stream.

use std::path::PathBuf;

use crate::git_util::is_safe_git_path;
use crate::path_util::{normalize_to_forward_slashes, path_from_bytes};

/// Parse `git status --porcelain=v1 -z` output into repo-relative paths.
///
/// A complete `status -z` output always ends in NUL, so a buffer that does
/// not was torn by a deadline kill (`GitOutput::Partial`): the final token is
/// a truncated record and is dropped. A torn path prefix such as `src/ma`
/// would otherwise pass `is_safe_git_path` and inflate the files-behind
/// estimate with a path that does not exist.
///
/// Skipped records: empty tokens, `## ...` branch headers (only emitted with
/// `--branch`, which the caller disables), `!!` ignored entries (only emitted
/// with `--ignored`, never passed), and any token that does not fit the
/// grammar (fewer than 4 bytes, or byte 2 not a space). Every other `XY` is
/// accepted without a whitelist: submodules emit lowercase `m` and `?` in the
/// second column, and a future git may add letters.
///
/// The same path can appear twice (`git rm --cached` yields `D ` and `??` for
/// one file). Deduplication is the caller's job (`ChangeSet.paths` is a set).
pub(super) fn parse_status_z(bytes: &[u8]) -> Vec<PathBuf> {
    let body = match bytes.last() {
        None => return Vec::new(),
        Some(0) => bytes,
        // Torn final record: drop everything after the last NUL.
        Some(_) => match bytes.iter().rposition(|&b| b == 0) {
            Some(last_nul) => &bytes[..=last_nul],
            None => return Vec::new(),
        },
    };

    let mut paths = Vec::new();
    let mut tokens = body.split(|&b| b == 0).peekable();
    while let Some(tok) = tokens.next() {
        if tok.is_empty() || tok[0] == b'#' {
            continue;
        }
        if tok.len() < 4 || tok[2] != b' ' {
            continue;
        }
        let xy = &tok[..2];
        if xy == b"!!" {
            continue;
        }
        push_path(&mut paths, &tok[3..]);
        // Rename/copy: the original path follows as its own record. Consume it
        // so the stream stays aligned, and report it too (it is a deletion).
        if xy.iter().any(|&b| b == b'R' || b == b'C') {
            if let Some(orig) = tokens.next() {
                push_path(&mut paths, orig);
            }
        }
    }
    paths
}

/// Same safety pipeline the old NUL-list parser applied to every git path:
/// bytes -> `PathBuf`, reject traversal/absolute, normalize separators so the
/// same file reported by two records collapses to one set entry.
fn push_path(paths: &mut Vec<PathBuf>, raw: &[u8]) {
    let path = path_from_bytes(raw);
    if is_safe_git_path(&path) {
        paths.push(normalize_to_forward_slashes(path));
    }
}

#[cfg(test)]
#[path = "porcelain_tests.rs"]
mod tests;
