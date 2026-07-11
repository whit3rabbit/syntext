//! Repo-relative path resolution for `notify_change`/`notify_delete`.
//!
//! Split from `mod.rs` to keep it under the 400-line quality gate. Child module
//! of `index`; `repo_relative_path` is `pub(super)` so `mod.rs` and the sibling
//! `update` module reach it, while the symlink helpers stay private here.

use std::path::{Component, Path};

use crate::IndexError;

impl super::Index {
    pub(super) fn repo_relative_path(&self, path: &Path) -> Result<std::path::PathBuf, IndexError> {
        // Strip either the configured repo_root or its canonicalized form.
        // Callers like `st update` canonicalize paths before calling
        // notify_change()/notify_delete() (to defend against symlink-based path
        // injection), so the incoming `path` may be absolute under
        // `canonical_root` even when `config.repo_root` was supplied via a
        // symlink (macOS /tmp, container bind-mounts). Stripping only
        // `config.repo_root` would fail `strip_prefix` and wrongly report
        // `PathOutsideRepo` for every changed file. Trying both prefixes keeps a
        // single consistent stripping rule regardless of how the caller reached
        // the repo.
        let rel = path
            .strip_prefix(&self.config.repo_root)
            .or_else(|_| path.strip_prefix(&self.canonical_root))
            .map_err(|_| IndexError::PathOutsideRepo(path.to_path_buf()))?;
        if rel.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(IndexError::PathOutsideRepo(path.to_path_buf()));
        }

        // Force forward slashes: this is an ingestion boundary. On Windows a
        // caller that canonicalizes first (`apply_changed_paths`, the durable
        // delta path) yields a backslash-separated `rel`, which would store
        // "src\foo.rs" in the segment and leak backslashes into search results.
        // Segments must always hold forward-slash paths (see CLAUDE.md).
        self.normalize_repo_relative_path(rel)
            .map(crate::path_util::normalize_to_forward_slashes)
    }

    fn normalize_repo_relative_path(&self, rel: &Path) -> Result<std::path::PathBuf, IndexError> {
        if !self.path_has_intermediate_symlink(rel)? {
            return Ok(rel.to_path_buf());
        }

        let abs = self.config.repo_root.join(rel);
        let Some(parent) = abs.parent() else {
            return Ok(rel.to_path_buf());
        };
        let canonical_parent = std::fs::canonicalize(parent)?;
        if !canonical_parent.starts_with(&self.canonical_root) {
            return Err(IndexError::PathOutsideRepo(abs));
        }

        let Some(file_name) = rel.file_name() else {
            return Ok(rel.to_path_buf());
        };
        let normalized = canonical_parent.join(file_name);
        normalized
            .strip_prefix(&self.canonical_root)
            .map(|p| p.to_path_buf())
            .map_err(|_| IndexError::PathOutsideRepo(normalized))
    }

    fn path_has_intermediate_symlink(&self, rel: &Path) -> Result<bool, IndexError> {
        let mut current = self.config.repo_root.clone();
        let mut components = rel.components().peekable();
        while let Some(component) = components.next() {
            let Component::Normal(part) = component else {
                continue;
            };
            if components.peek().is_none() {
                break;
            }
            current.push(part);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => return Ok(true),
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(err) => return Err(IndexError::Io(err)),
            }
        }

        Ok(false)
    }
}
