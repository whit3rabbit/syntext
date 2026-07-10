//! External-file-list build entry: caller-supplied file records.
//!
//! Split from `build.rs` to keep it under the 400-line quality gate. The heavy
//! lifting stays in `build::build_index_from_file_list`; this module owns only
//! the public `ExternalFileRecord` type and the normalization that feeds it in.

use std::path::{Component, PathBuf};

use crate::index::build::{build_index_from_file_list, BATCH_SIZE_BYTES};
use crate::index::walk::{FileRecord, WalkSkips};
use crate::{Config, IndexError};

/// A caller-supplied file admitted to a full rebuild without syntext walking
/// the repository itself.
///
/// The caller owns discovery policy; syntext trusts `absolute_path` to refer
/// to a readable file. TOCTOU defenses (`open_readonly_nofollow` +
/// `verify_fd_matches_stat`) still apply at read time.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExternalFileRecord {
    /// Absolute path read during index construction.
    pub absolute_path: PathBuf,
    /// Repository-relative path stored in the manifest and returned in matches.
    pub relative_path: PathBuf,
    /// Size of the file in bytes at discovery time.
    pub size_bytes: u64,
}

/// Full build from a caller-supplied file list.
pub(super) fn build_index_from_external_records(
    config: Config,
    records: Vec<ExternalFileRecord>,
) -> Result<super::Index, IndexError> {
    build_index_from_file_list(
        config,
        normalize_external_records(records)?,
        WalkSkips::default(),
        BATCH_SIZE_BYTES,
    )
}

fn normalize_external_records(
    records: Vec<ExternalFileRecord>,
) -> Result<Vec<FileRecord>, IndexError> {
    let mut file_list = Vec::with_capacity(records.len());
    for record in records {
        if record.relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(IndexError::PathOutsideRepo(record.relative_path));
        }

        file_list.push((
            record.absolute_path,
            crate::path_util::normalize_to_forward_slashes(record.relative_path),
            record.size_bytes,
        ));
    }
    file_list.sort_unstable_by(|left, right| left.1.cmp(&right.1));
    Ok(file_list)
}
