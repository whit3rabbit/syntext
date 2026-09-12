//! Persistent file catalogue tracking content digests and generation progress.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::batch::{ChangeBatch, ChangeKind, ChangeRecord};
use super::codec::{read_catalogue, write_catalogue, CatalogueEntry};
use super::fingerprint::{hash_file, FileFingerprint, StatObservation};

/// Default maximum file size whose content bytes are retained in memory
/// during observation (256 KiB).
pub const DEFAULT_MAX_RETAIN_BYTES: usize = 256 * 1024;

/// Filename of the catalogue sidecar inside the index directory.
pub const CATALOGUE_FILENAME: &str = "catalogue.idx";

/// A persistent file catalogue that maps repository paths to settled content
/// fingerprints, observed generations, and consumer acknowledgement positions.
pub struct Catalogue {
    catalogue_path: PathBuf,
    generation: u64,
    entries: HashMap<PathBuf, CatalogueEntry>,
    consumers: HashMap<String, u64>,
    max_retain_bytes: usize,
}

impl Catalogue {
    /// Open or create a catalogue inside `index_dir`.
    pub fn open_or_create(index_dir: &Path) -> io::Result<Self> {
        let catalogue_path = index_dir.join(CATALOGUE_FILENAME);
        if let Some(decoded) = read_catalogue(&catalogue_path)? {
            Ok(Self {
                catalogue_path,
                generation: decoded.generation,
                entries: decoded.entries,
                consumers: decoded.consumers,
                max_retain_bytes: DEFAULT_MAX_RETAIN_BYTES,
            })
        } else {
            Ok(Self {
                catalogue_path,
                generation: 0,
                entries: HashMap::new(),
                consumers: HashMap::new(),
                max_retain_bytes: DEFAULT_MAX_RETAIN_BYTES,
            })
        }
    }

    /// Set the in-memory content retention limit (in bytes) for small files.
    pub fn set_max_retain_bytes(&mut self, bytes: usize) {
        self.max_retain_bytes = bytes;
    }

    /// Current monotonic generation of the catalogue.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of tracked files in the catalogue.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no files are tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Retrieve the recorded fingerprint for a repository-relative path.
    pub fn get_fingerprint(&self, rel_path: &Path) -> Option<&FileFingerprint> {
        self.entries.get(rel_path).map(|e| &e.fingerprint)
    }

    /// Observe a set of candidate paths against the catalogue and disk.
    /// Returns a `ChangeBatch` tagging changes with an incremented generation
    /// if any real additions, modifications, or deletions were found.
    pub fn observe_paths(
        &mut self,
        root: &Path,
        candidate_paths: &[PathBuf],
        read_epoch: SystemTime,
    ) -> ChangeBatch {
        let now = SystemTime::now();
        let mut records = Vec::with_capacity(candidate_paths.len());
        let mut content_changed = false;
        let mut has_uncertainty = false;

        for raw_path in candidate_paths {
            let rel = normalize_rel_path(root, raw_path);
            let abs = root.join(&rel);

            let stat = match StatObservation::stat(&abs) {
                Some(s) => s,
                None => {
                    // Could not stat (e.g. permissions or vanishing unreadable entry).
                    let prev_fp = self.get_fingerprint(&rel).copied();
                    records.push(ChangeRecord {
                        path: rel,
                        kind: ChangeKind::Uncertain,
                        current_fingerprint: None,
                        previous_fingerprint: prev_fp,
                        content: None,
                    });
                    continue;
                }
            };

            match stat {
                StatObservation::Absent => {
                    if let Some(prev) = self.entries.remove(&rel) {
                        content_changed = true;
                        records.push(ChangeRecord {
                            path: rel,
                            kind: ChangeKind::Deleted,
                            current_fingerprint: None,
                            previous_fingerprint: Some(prev.fingerprint),
                            content: None,
                        });
                    }
                }
                StatObservation::Present { .. } => {
                    let prev = self.entries.get(&rel).cloned();
                    if let Some(ref prev_entry) = prev {
                        // Fast path: if size and timestamp match and are settled, content is unchanged.
                        if stat.matches_fingerprint(&prev_entry.fingerprint)
                            && prev_entry.fingerprint.is_settled(read_epoch, now)
                        {
                            records.push(ChangeRecord {
                                path: rel,
                                kind: ChangeKind::Unchanged,
                                current_fingerprint: Some(prev_entry.fingerprint),
                                previous_fingerprint: Some(prev_entry.fingerprint),
                                content: None,
                            });
                            continue;
                        }
                    }

                    // Stat changed or was unsettled: hash the file contents.
                    match hash_file(&abs, self.max_retain_bytes) {
                        Ok(hashed) => {
                            let fp = hashed.fingerprint;
                            if let Some(ref prev_entry) = prev {
                                if prev_entry.fingerprint.digest == fp.digest {
                                    // Content identical, only metadata shifted (e.g. touch).
                                    self.entries.insert(
                                        rel.clone(),
                                        CatalogueEntry {
                                            fingerprint: fp,
                                            observed_generation: prev_entry.observed_generation,
                                        },
                                    );
                                    records.push(ChangeRecord {
                                        path: rel,
                                        kind: ChangeKind::Touched,
                                        current_fingerprint: Some(fp),
                                        previous_fingerprint: Some(prev_entry.fingerprint),
                                        content: hashed.buffer,
                                    });
                                    continue;
                                }
                            }

                            // Digest is new or file was newly added.
                            content_changed = true;
                            let kind = if prev.is_some() {
                                ChangeKind::Modified
                            } else {
                                ChangeKind::Added
                            };

                            let next_gen = self.generation + 1;
                            self.entries.insert(
                                rel.clone(),
                                CatalogueEntry {
                                    fingerprint: fp,
                                    observed_generation: next_gen,
                                },
                            );
                            records.push(ChangeRecord {
                                path: rel,
                                kind,
                                current_fingerprint: Some(fp),
                                previous_fingerprint: prev.map(|p| p.fingerprint),
                                content: hashed.buffer,
                            });
                        }
                        Err(_) => {
                            has_uncertainty = true;
                            records.push(ChangeRecord {
                                path: rel,
                                kind: ChangeKind::Uncertain,
                                current_fingerprint: None,
                                previous_fingerprint: prev.map(|p| p.fingerprint),
                                content: None,
                            });
                        }
                    }
                }
            }
        }

        if content_changed {
            self.generation += 1;
        }

        ChangeBatch::new(self.generation, records, has_uncertainty)
    }

    /// Record a consumer's acknowledgement of processing up to `generation`.
    pub fn acknowledge(&mut self, consumer: &str, generation: u64) {
        let entry = self.consumers.entry(consumer.to_string()).or_insert(0);
        if generation > *entry {
            *entry = generation;
        }
    }

    /// Query the lag of `consumer` behind current catalogue generation.
    pub fn consumer_lag(&self, consumer: &str) -> u64 {
        let applied = self.consumers.get(consumer).copied().unwrap_or(0);
        self.generation.saturating_sub(applied)
    }

    /// Persist catalogue state to disk atomically.
    pub fn save(&self) -> io::Result<()> {
        write_catalogue(
            &self.catalogue_path,
            self.generation,
            &self.entries,
            &self.consumers,
        )
    }
}

fn normalize_rel_path(root: &Path, path: &Path) -> PathBuf {
    let rel = if path.is_absolute() {
        path.strip_prefix(root).unwrap_or(path)
    } else {
        path
    };
    crate::path_util::normalize_to_forward_slashes(rel.to_path_buf())
}

#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod tests;
