//! Repository walking and file utilities.

use std::path::PathBuf;

use ignore::WalkBuilder;

use crate::{Config, IndexError};

pub type FileRecord = (PathBuf, String, u64);

/// Walk the repository collecting indexable files. Respects `.gitignore`.
pub fn enumerate_files(config: &Config) -> Result<Vec<FileRecord>, IndexError> {
    let mut files: Vec<FileRecord> = Vec::new();

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
        if !path.is_file() {
            continue;
        }
        let size = match path.metadata() {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if size > config.max_file_size {
            continue;
        }
        let rel = match path.strip_prefix(&config.repo_root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => continue,
        };
        // Normalize path separators to forward slashes for consistency.
        let rel = rel.replace('\\', "/");
        files.push((path.to_path_buf(), rel, size));
    }

    files.sort_unstable_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
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

/// Returns `true` if content has a null byte in the first 8KB (binary heuristic).
pub fn is_binary(content: &[u8]) -> bool {
    let check = content.len().min(8192);
    content[..check].contains(&0u8)
}
