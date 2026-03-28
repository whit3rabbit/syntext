//! Repository walking and file utilities.

use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::{Config, IndexError};

/// A record of a scanned file pending indexing: `(absolute_path, relative_path, size_bytes)`.
pub type FileRecord = (PathBuf, String, u64);

/// Walk the repository collecting indexable files. Respects `.gitignore`.
pub fn enumerate_files(config: &Config) -> Result<Vec<FileRecord>, IndexError> {
    let mut files: Vec<FileRecord> = Vec::new();
    let canonical_root = fs::canonicalize(&config.repo_root)?;

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
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => return,
    };
    let rel = rel.replace('\\', "/");
    files.push((read_path, rel, size));
}

fn collect_symlink_entry(
    symlink_path: &Path,
    repo_root: &Path,
    canonical_root: &Path,
    max_file_size: u64,
    files: &mut Vec<FileRecord>,
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

    if target_meta.is_file() {
        push_file_record(
            canonical_target,
            symlink_path,
            repo_root,
            max_file_size,
            files,
        );
        return;
    }

    if !target_meta.is_dir() {
        return;
    }

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
            files.iter().any(|(_, rel, _)| rel == "alias/nested.rs"),
            "symlinked directory contents should be indexed via the symlink path"
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
}
