//! Repository walking and file utilities.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::{Config, IndexError};

/// Maximum number of distinct symlinked directories resolved during a single
/// `enumerate_files` call. Each symlinked directory spawns a nested sub-walk;
/// an unbounded number of distinct symlinks to distinct directories could
/// stall indexing indefinitely. This cap prevents that.
pub(crate) const MAX_SYMLINK_WALKERS: usize = 256;

/// A record of a scanned file pending indexing: `(absolute_path, relative_path, size_bytes)`.
pub type FileRecord = (PathBuf, PathBuf, u64);

/// Walk the repository collecting indexable files. Respects `.gitignore`.
pub fn enumerate_files(config: &Config) -> Result<Vec<FileRecord>, IndexError> {
    let mut files: Vec<FileRecord> = Vec::new();
    let canonical_root = fs::canonicalize(&config.repo_root)?;
    // Track canonical paths already queued via symlinks so that N symlinks
    // pointing at the same target don't produce N copies of its contents.
    let mut seen_canonical: HashSet<PathBuf> = HashSet::new();

    let walker = WalkBuilder::new(&config.repo_root)
        .hidden(false) // include hidden files (gitignore handles exclusions)
        .git_ignore(true)
        .follow_links(false)
        .build();

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
            collect_symlink_entry(
                path,
                &config.repo_root,
                &canonical_root,
                config.max_file_size,
                &mut files,
                &mut seen_canonical,
            );
            continue;
        }

        if !file_type.is_file() {
            continue;
        }
        push_file_record(
            path.to_path_buf(),
            path,
            &config.repo_root,
            config.max_file_size,
            &mut files,
        );
    }

    files.sort_unstable_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

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
        Ok(r) => r.to_path_buf(),
        Err(_) => return,
    };
    files.push((read_path, rel, size));
}

/// Resolve a symlink entry and add it to the file list if it points inside the repo.
///
/// Security audit (symlink escape): three-layer defense prevents indexing files
/// outside the repository root:
///   1. `canonicalize(target)` resolves the full chain, then `starts_with(canonical_root)`
///      rejects targets outside the repo.
///   2. Post-canonicalize TOCTOU guard: `symlink_metadata(canonical_target)` re-stats
///      the resolved path and rejects it if it is itself a symlink (concurrent swap).
///   3. `seen_canonical` deduplication prevents N symlinks to the same directory from
///      producing N sub-walks, bounding traversal cost.
///
/// Multi-hop symlink chains are rejected at step 1 (the initial `symlink_metadata`
/// check rejects targets that are themselves symlinks, limiting to one level of
/// indirection). Tests: `enumerate_files_skips_symlink_outside_repo`,
/// `collect_symlink_entry_rejects_canonical_symlink`.
fn collect_symlink_entry(
    symlink_path: &Path,
    repo_root: &Path,
    canonical_root: &Path,
    max_file_size: u64,
    files: &mut Vec<FileRecord>,
    seen_canonical: &mut HashSet<PathBuf>,
) {
    let target = match fs::read_link(symlink_path) {
        Ok(target) => target,
        Err(_) => return,
    };
    let target_path = if target.is_absolute() {
        target
    } else {
        symlink_path.parent().unwrap_or(repo_root).join(target)
    };
    let target_meta = match fs::symlink_metadata(&target_path) {
        Ok(meta) => meta,
        Err(_) => return,
    };
    if target_meta.file_type().is_symlink() {
        return;
    }

    let canonical_target = match fs::canonicalize(&target_path) {
        Ok(path) => path,
        Err(_) => return,
    };
    if !canonical_target.starts_with(canonical_root) {
        return;
    }

    // Guard against TOCTOU: re-stat the canonical path after canonicalize().
    // A concurrent symlink swap between symlink_metadata() and canonicalize()
    // could redirect to a symlink that passes starts_with(canonical_root).
    // If the canonical path is itself a symlink, reject it.
    let canonical_meta = match fs::symlink_metadata(&canonical_target) {
        Ok(meta) => meta,
        Err(_) => return,
    };
    if canonical_meta.file_type().is_symlink() {
        return;
    }

    // Without this, N symlinks to the same directory produce N full sub-walks.
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
        return;
    }

    if !canonical_meta.is_dir() {
        return;
    }

    // Cap the number of distinct directory sub-walks to prevent pathological
    // repos with many symlinks from stalling indexing indefinitely.
    if seen_canonical.len() >= MAX_SYMLINK_WALKERS {
        return;
    }
    seen_canonical.insert(canonical_target.clone());
    let nested = WalkBuilder::new(&canonical_target)
        .hidden(false)
        .git_ignore(true)
        .follow_links(false)
        .build();

    for result in nested {
        let entry = match result {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let Ok(suffix) = entry.path().strip_prefix(&canonical_target) else {
            continue;
        };
        let display_path = symlink_path.join(suffix);
        push_file_record(
            entry.path().to_path_buf(),
            &display_path,
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn enumerate_files_deduplicates_multiple_symlinks_to_same_dir() {
        use std::os::unix::fs::symlink;

        let repo = TempDir::new().unwrap();
        let real_dir = repo.path().join("real");
        fs::create_dir_all(&real_dir).unwrap();
        for i in 0..5u8 {
            fs::write(real_dir.join(format!("file{i}.rs")), b"fn f() {}").unwrap();
        }
        // 10 symlinks all pointing at the same 5-file directory
        for i in 0..10u8 {
            symlink(&real_dir, repo.path().join(format!("alias{i}"))).unwrap();
        }

        let config = Config {
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };

        let files = enumerate_files(&config).unwrap();
        // Without dedup we'd get 5 + 10*5 = 55; with dedup exactly 10.
        assert_eq!(
            files.len(),
            10,
            "5 real + 5 via alias0, got {}",
            files.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn enumerate_files_follows_symlinked_directory_one_level() {
        use std::os::unix::fs::symlink;

        let repo = TempDir::new().unwrap();
        let nested_dir = repo.path().join("real");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::write(nested_dir.join("nested.rs"), b"fn linked() {}\n").unwrap();
        symlink(&nested_dir, repo.path().join("alias")).unwrap();

        let config = Config {
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };

        let files = enumerate_files(&config).unwrap();
        assert!(
            files
                .iter()
                .any(|(_, rel, _)| rel == &PathBuf::from("alias/nested.rs")),
            "symlinked directory contents should be indexed via the symlink path"
        );
    }

    #[cfg(unix)]
    #[test]
    fn collect_symlink_entry_rejects_canonical_symlink() {
        // Simulates: symlink in repo -> dir inside repo, but that dir was
        // replaced with a symlink to an outside location (post-canonicalize race).
        // We test the defense: after canonicalize, if the result is itself a
        // symlink, it must be rejected.
        use std::os::unix::fs::symlink;

        let repo = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();

        // real file outside repo
        std::fs::write(outside.path().join("secret.rs"), b"secret").unwrap();

        // link_b inside repo -> outside/secret.rs (so canonical_target is outside root)
        symlink(outside.path().join("secret.rs"), repo.path().join("link_b")).unwrap();

        // link_a -> link_b (a chain: canonicalize of link_a resolves to outside/secret.rs)
        symlink(repo.path().join("link_b"), repo.path().join("link_a")).unwrap();

        let config = crate::Config {
            repo_root: repo.path().to_path_buf(),
            ..crate::Config::default()
        };

        let files = enumerate_files(&config).unwrap();
        // Neither link_a nor link_b should appear in results (both lead outside repo).
        let found: Vec<_> = files
            .iter()
            .filter(|(_, rel, _)| rel.starts_with("link_a") || rel.starts_with("link_b"))
            .collect();
        assert!(
            found.is_empty(),
            "symlinks pointing outside repo must be rejected, found: {:?}",
            found
        );
    }

    #[cfg(unix)]
    #[test]
    fn enumerate_files_skips_symlink_outside_repo() {
        use std::os::unix::fs::symlink;

        let repo = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret.rs"), b"fn secret() {}\n").unwrap();
        symlink(
            outside.path().join("secret.rs"),
            repo.path().join("escape.rs"),
        )
        .unwrap();

        let config = Config {
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };

        let files = enumerate_files(&config).unwrap();
        assert!(
            !files.iter().any(|(_, rel, _)| rel == "escape.rs"),
            "out-of-repo symlink targets must be skipped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn enumerate_files_caps_symlink_walkers() {
        use std::os::unix::fs::symlink;

        let repo = TempDir::new().unwrap();
        let n = super::MAX_SYMLINK_WALKERS + 5;
        for i in 0..n {
            let dir = repo.path().join(format!("real_dir_{i}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("f.rs"), b"fn f() {}").unwrap();
            symlink(&dir, repo.path().join(format!("link_{i}"))).unwrap();
        }

        let config = Config {
            repo_root: repo.path().to_path_buf(),
            ..Config::default()
        };

        let files = enumerate_files(&config).unwrap();
        let symlinked: Vec<_> = files
            .iter()
            .filter(|(_, rel, _)| rel.to_str().unwrap_or("").starts_with("link_"))
            .collect();
        assert!(
            symlinked.len() <= super::MAX_SYMLINK_WALKERS,
            "must not process more than MAX_SYMLINK_WALKERS symlinked dirs, got {}",
            symlinked.len()
        );
    }
}
