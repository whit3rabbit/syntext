//! Syncing a store with a directory: the whole tree, or only paths the
//! caller knows changed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use syntext::changes::{Catalogue, ChangeKind};

use super::directory::{keep, read_capped, rel_string, CONSUMER, OBSERVE_CHUNK};
use super::{decode_text, DirectorySource, SourceDoc, SyncReport};
use crate::error::{ContextError, Result};
use crate::store::ContextStore;
use crate::types::{validate_label, DocKey};

/// Progress of a sync, reported after each chunk of observed paths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SyncProgress {
    /// Paths checked against the change catalogue so far.
    pub observed: usize,
    /// Paths this sync checks.
    pub total: usize,
    /// Changed files read and queued for compilation so far.
    pub changed: usize,
}

impl ContextStore {
    /// Bring the documents under `source.label` in line with the directory.
    ///
    /// Added and modified files are compiled; deleted, newly ignored, and
    /// excluded files are removed; everything lands in one generation.
    /// Files are read and compiled a batch at a time, so memory stays
    /// bounded on a first sync of a large tree. The change catalogue is
    /// saved only after that generation is durable, so an interrupted sync
    /// is simply redone.
    pub fn sync_directory(&self, source: &DirectorySource) -> Result<SyncReport> {
        self.sync_directory_with(source, |_| true)
    }

    /// [`sync_directory`](Self::sync_directory), calling `progress` after
    /// each chunk of observed paths. Returning `false` cancels: the result
    /// is [`ContextError::Cancelled`] and nothing is published.
    pub fn sync_directory_with(
        &self,
        source: &DirectorySource,
        progress: impl FnMut(&SyncProgress) -> bool,
    ) -> Result<SyncReport> {
        self.sync(source, None, progress)
    }

    /// Bring only `paths` in line with the directory, without walking it:
    /// changed files are recompiled, and deleted or excluded ones removed.
    /// Paths may be absolute (under the source root) or relative to it;
    /// others and directories are ignored. `.gitignore` is not consulted,
    /// so pass paths a full sync would include, such as files the
    /// application itself just wrote; the next full sync reconciles the
    /// rest.
    pub fn sync_paths(
        &self,
        source: &DirectorySource,
        paths: &[impl AsRef<Path>],
    ) -> Result<SyncReport> {
        let paths: Vec<PathBuf> = paths.iter().map(|p| p.as_ref().to_path_buf()).collect();
        self.sync(source, Some(&paths), |_| true)
    }

    fn sync(
        &self,
        source: &DirectorySource,
        only: Option<&[PathBuf]>,
        mut progress: impl FnMut(&SyncProgress) -> bool,
    ) -> Result<SyncReport> {
        validate_label(&source.label)?;
        if self.is_read_only() {
            return Err(ContextError::ReadOnly);
        }
        let root = std::fs::canonicalize(&source.root)?;
        let store_rel = std::fs::canonicalize(self.dir())
            .ok()
            .and_then(|s| s.strip_prefix(&root).ok().map(Path::to_path_buf));
        let kept = |rel: &String| keep(rel, source, store_rel.as_deref());
        // `candidates` are the paths observed; `discovered` those of them
        // that exist and belong in the store.
        let (candidates, discovered, too_large) = match only {
            None => {
                let mut cfg = syntext::Config::new(self.dir().to_path_buf(), root.clone());
                cfg.max_file_size = source.max_file_bytes;
                cfg.index_nested_checkouts = source.include_nested_checkouts;
                let (records, skips) = syntext::index::walk::enumerate_files(&cfg)?;
                let discovered: BTreeSet<String> = records
                    .iter()
                    .filter_map(|(_, rel, _)| rel_string(rel))
                    .filter(kept)
                    .collect();
                (None, discovered, skips.too_large)
            }
            Some(paths) => {
                let rels: BTreeSet<String> = paths
                    .iter()
                    .filter_map(|p| relative_to(&root, &source.root, p))
                    .collect();
                let discovered = rels
                    .iter()
                    .filter(|rel| kept(rel))
                    .filter(|rel| {
                        std::fs::symlink_metadata(root.join(rel.as_str()))
                            .is_ok_and(|m| m.is_file())
                    })
                    .cloned()
                    .collect();
                (Some(rels), discovered, 0)
            }
        };

        let catalogue_dir = self.dir().join("sources").join(&source.label);
        std::fs::create_dir_all(&catalogue_dir)?;
        let mut catalogue = Catalogue::open_or_create(&catalogue_dir)?;
        let mut report = SyncReport {
            scanned: candidates.as_ref().map_or(discovered.len(), BTreeSet::len),
            skipped: too_large,
            ..SyncReport::default()
        };
        let prefix = format!("{}:", source.label);
        let ingest = self.write_batched(|staged| {
            // Taken under the writer lock: no other write can interleave.
            let snap = self.snapshot();
            let known: BTreeSet<String> = snap
                .documents()
                .into_iter()
                .filter_map(|d| d.key.strip_prefix(&prefix).map(str::to_string))
                .collect();
            let observe: Vec<String> = match &candidates {
                Some(rels) => rels.iter().cloned().collect(),
                None => discovered.union(&known).cloned().collect(),
            };
            let mut state = SyncProgress {
                total: observe.len(),
                ..SyncProgress::default()
            };
            let mut removals = Vec::new();
            for chunk in observe.chunks(OBSERVE_CHUNK) {
                let paths: Vec<PathBuf> = chunk.iter().map(PathBuf::from).collect();
                let batch = catalogue.observe_paths(&root, &paths, SystemTime::now());
                for rec in batch.records {
                    let Some(rel) = rel_string(&rec.path) else {
                        continue;
                    };
                    match rec.kind {
                        ChangeKind::Added | ChangeKind::Modified if discovered.contains(&rel) => {
                            let key = DocKey::for_file(&source.label, &rel)?;
                            let digest = rec.digest().copied();
                            let unchanged_in_store = digest.is_some_and(|d| {
                                snap.document(key.as_str())
                                    .is_some_and(|s| s.source_hash == d.to_hex())
                            });
                            if unchanged_in_store {
                                continue;
                            }
                            let read;
                            let bytes = match &rec.content {
                                Some(c) => Some(&c[..]),
                                None => {
                                    read = read_capped(&root.join(&rel), source.max_file_bytes);
                                    read.as_deref()
                                }
                            };
                            let Some(text) = bytes.and_then(decode_text) else {
                                // Now binary or unreadable: the old text goes.
                                report.skipped += 1;
                                if known.contains(&rel) {
                                    removals.push(key);
                                }
                                continue;
                            };
                            let mut doc = SourceDoc::file(&source.label, &rel, text)?
                                .with_scope(source.scope.clone())
                                .with_trust(source.trust);
                            if let Some(fp) = rec.current_fingerprint {
                                doc = doc
                                    .with_timestamp(fp.mtime_secs)
                                    .with_source_hash(fp.digest);
                            } else if let Some(d) = digest {
                                doc = doc.with_source_hash(d);
                            }
                            if known.contains(&rel) {
                                report.modified += 1;
                            } else {
                                report.added += 1;
                            }
                            state.changed += 1;
                            staged.push(doc)?;
                        }
                        ChangeKind::Uncertain => report.uncertain += 1,
                        _ => {}
                    }
                }
                state.observed += chunk.len();
                if !progress(&state) {
                    return Err(ContextError::Cancelled);
                }
            }
            // Known documents that no longer exist or belong (deleted, newly
            // ignored, or excluded) go too.
            for rel in &observe {
                if known.contains(rel) && !discovered.contains(rel) {
                    removals.push(DocKey::for_file(&source.label, rel)?);
                }
            }
            Ok(removals)
        })?;
        report.removed = ingest.removed;
        report.skipped += ingest.skipped.len();
        report.generation = ingest.generation;
        catalogue.acknowledge(CONSUMER, catalogue.generation());
        catalogue.save()?;
        Ok(report)
    }
}

/// `path` relative to the source root (canonical or as configured), as a
/// forward-slash string.
fn relative_to(root: &Path, configured: &Path, path: &Path) -> Option<String> {
    if path.is_relative() {
        return rel_string(path);
    }
    path.strip_prefix(root)
        .or_else(|_| path.strip_prefix(configured))
        .ok()
        .and_then(rel_string)
}
