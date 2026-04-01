//! Repository walking and file utilities.

#[cfg(feature = "ignore")]
use std::collections::HashSet;
#[cfg(feature = "ignore")]
use std::fs;
#[cfg(feature = "ignore")]
use std::path::Path;
use std::path::PathBuf;

#[cfg(feature = "ignore")]
use ignore::WalkBuilder;

#[cfg(feature = "ignore")]
use crate::{Config, IndexError};

/// A record of a scanned file pending indexing: `(absolute_path, relative_path, size_bytes)`.
pub type FileRecord = (PathBuf, PathBuf, u64);

/// Walk the repository collecting indexable files. Respects `.gitignore`.
#[cfg(feature = "ignore")]
pub fn enumerate_files(config: &Config) -> Result<Vec<FileRecord>, IndexError> {
    let mut files: Vec<FileRecord> = Vec::new();
    let canonical_root = fs::canonicalize(&config.repo_root)?;
    // Track canonical paths so that symlinks to already-indexed files are
    // skipped.  Regular files are processed first (pass 1) so that a real
    // file always wins over any symlink alias to it, regardless of walk order.
    let mut seen_canonical: HashSet<PathBuf> = HashSet::new();

    let walker = WalkBuilder::new(&config.repo_root)
        .hidden(false) // include hidden files (gitignore handles exclusions)
        .git_ignore(true)
        .follow_links(false)
        .build();

    // Pass 1: regular files.  Buffer symlink paths for pass 2.
    let mut symlink_paths: Vec<PathBuf> = Vec::new();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue, // skip unreadable entries
        };
        let path = entry.path();
        let Some(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_symlink() {
            symlink_paths.push(path.to_path_buf());
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        // Register the canonical path so that any symlink pointing here is
        // deduplicated in pass 2.  If canonicalize fails (e.g. a race with
        // deletion) we still index the file rather than silently dropping it.
        if let Ok(canonical) = fs::canonicalize(path) {
            seen_canonical.insert(canonical);
        }
        push_file_record(
            path.to_path_buf(),
            path,
            &config.repo_root,
            config.max_file_size,
            &mut files,
        );
    }

    // Pass 2: symlinks.  seen_canonical is now fully populated from all
    // regular files, so any alias pointing to an already-indexed file is
    // dropped instead of creating a duplicate index entry.
    for symlink_path in symlink_paths {
        collect_symlink_entry(
            &symlink_path,
            &config.repo_root,
            &canonical_root,
            config.max_file_size,
            &mut files,
            &mut seen_canonical,
            config.verbose,
        );
    }

    files.sort_unstable_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

#[cfg(feature = "ignore")]
fn push_file_record(
    read_path: PathBuf,
    display_path: &Path,
    repo_root: &Path,
    max_file_size: u64,
    files: &mut Vec<FileRecord>,
) {
    let size = match read_path.metadata() {
        Ok(m) => m.len(),
        Err(_) => return,
    };
    if size > max_file_size {
        return;
    }
    let rel = match display_path.strip_prefix(repo_root) {
        Ok(r) => crate::path_util::normalize_to_forward_slashes(r.to_path_buf()),
        Err(_) => return,
    };
    files.push((read_path, rel, size));
}

/// Resolve a symlink entry and add it to the file list if it points inside the repo.
///
/// Directory symlinks are skipped. This keeps the default indexed corpus aligned
/// with default `rg`, which does not recurse through symlinked directories unless
/// `-L/--follow` is requested.
///
/// Security audit (symlink escape): three-layer defense prevents indexing files
/// outside the repository root:
///   1. `canonicalize(target)` resolves the full chain, then `starts_with(canonical_root)`
///      rejects targets outside the repo.
///   2. Post-canonicalize TOCTOU guard: `symlink_metadata(canonical_target)` re-stats
///      the resolved path and rejects it if it is itself a symlink (concurrent swap).
///   3. `seen_canonical` deduplication prevents N symlinks to the same file target from
///      producing duplicate file records.
///
/// Multi-hop symlink chains: the immediate-target `symlink_metadata` check
/// (step 1) rejects targets that are themselves symlinks, which covers chains
/// where every hop is a plain symlink name.  However, `canonicalize` at step 2
/// follows the full chain regardless; the escape guard is
/// `starts_with(canonical_root)` applied to the fully resolved path.  The
/// per-hop check is defence-in-depth, not the primary protection.
/// Tests: `enumerate_files_skips_symlink_outside_repo`,
/// `collect_symlink_entry_rejects_canonical_symlink`.
#[cfg(feature = "ignore")]
fn log_symlink_skip(verbose: bool, symlink_path: &Path, reason: std::fmt::Arguments<'_>) {
    if verbose {
        eprintln!("st: skipping symlink {}: {reason}", symlink_path.display());
    }
}

#[cfg(feature = "ignore")]
fn collect_symlink_entry(
    symlink_path: &Path,
    repo_root: &Path,
    canonical_root: &Path,
    max_file_size: u64,
    files: &mut Vec<FileRecord>,
    seen_canonical: &mut HashSet<PathBuf>,
    verbose: bool,
) {
    let target = match fs::read_link(symlink_path) {
        Ok(target) => target,
        Err(e) => {
            log_symlink_skip(verbose, symlink_path, format_args!("failed to read link: {e}"));
            return;
        }
    };
    let target_path = if target.is_absolute() {
        target
    } else {
        symlink_path.parent().unwrap_or(repo_root).join(target)
    };
    let target_meta = match fs::symlink_metadata(&target_path) {
        Ok(meta) => meta,
        Err(e) => {
            log_symlink_skip(verbose, symlink_path, format_args!("failed to stat target: {e}"));
            return;
        }
    };
    if target_meta.file_type().is_symlink() {
        return;
    }

    let canonical_target = match fs::canonicalize(&target_path) {
        Ok(path) => path,
        Err(e) => {
            log_symlink_skip(verbose, symlink_path, format_args!("failed to canonicalize target: {e}"));
            return;
        }
    };
    if !canonical_target.starts_with(canonical_root) {
        log_symlink_skip(
            verbose,
            symlink_path,
            format_args!("target {} is outside repo root", canonical_target.display()),
        );
        return;
    }

    // Guard against TOCTOU: re-stat the canonical path after canonicalize().
    // A concurrent symlink swap between symlink_metadata() and canonicalize()
    // could redirect to a symlink that passes starts_with(canonical_root).
    // If the canonical path is itself a symlink, reject it.
    let canonical_meta = match fs::symlink_metadata(&canonical_target) {
        Ok(meta) => meta,
        Err(e) => {
            log_symlink_skip(verbose, symlink_path, format_args!("failed to stat canonical target: {e}"));
            return;
        }
    };
    if canonical_meta.file_type().is_symlink() {
        return;
    }

    // Without this, N symlinks to the same file target produce duplicate records.
    if seen_canonical.contains(&canonical_target) {
        return;
    }

    if canonical_meta.is_file() {
        seen_canonical.insert(canonical_target.clone());
        push_file_record(
            canonical_target,
            symlink_path,
            repo_root,
            max_file_size,
            files,
        );
    }
}

/// Partition files into batches of approximately `batch_limit` bytes.
pub fn split_batches(files: &[FileRecord], batch_limit: u64) -> Vec<Vec<FileRecord>> {
    let mut batches: Vec<Vec<FileRecord>> = Vec::new();
    let mut current: Vec<FileRecord> = Vec::new();
    let mut current_size: u64 = 0;

    for record in files {
        let size = record.2;
        if !current.is_empty() && current_size + size > batch_limit {
            batches.push(std::mem::take(&mut current));
            current_size = 0;
        }
        // Cap accounting so a single oversized file doesn't cause the next
        // file to start a batch that already exceeds the limit.
        current_size += size.min(batch_limit);
        current.push(record.clone());
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// Heuristic binary file detector.
///
/// **Spec (CHK015):** A file is binary if any of the first 8192 bytes is a
/// null byte (0x00). This matches git/grep behaviour and is the canonical
/// definition for syntext's indexing pipeline. Binary files are never indexed.
pub fn is_binary(content: &[u8]) -> bool {
    let check = content.len().min(8192);
    content[..check].contains(&0u8)
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
