//! Index build pipeline: full-corpus segment construction.
//!
//! `build_index()` is the implementation of `Index::build()`. It is split out
//! to keep `mod.rs` under the 400-line quality gate. Everything here runs only
//! during a fresh `st index` build; it is not used by open/search/commit.

use std::fs;
use std::io::Read;
use std::path::{Component, PathBuf};

#[cfg(feature = "fs2")]
use fs2::FileExt;
#[cfg(feature = "rayon")]
use rayon::prelude::*;
use xxhash_rust::xxh64::xxh64;

use crate::index::calibrate::calibrate_threshold;
use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::segment::SegmentWriter;
#[cfg(feature = "ignore")]
use crate::index::walk::enumerate_files;
use crate::index::walk::is_binary;
use crate::index::walk::{split_batches, FileRecord};
use crate::tokenizer::build_all;
use crate::{Config, IndexError};

/// Target batch size (content bytes) before flushing a segment.
pub(super) const BATCH_SIZE_BYTES: u64 = 256 * 1024 * 1024;

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

/// Full-corpus index build. Called by `Index::build()`; returns a ready-to-use
/// `Index` by delegating to `Index::open()` after writing all segments.
#[cfg(feature = "ignore")]
pub(super) fn build_index(config: Config) -> Result<super::Index, IndexError> {
    let file_list = enumerate_files(&config)?;
    build_index_from_file_list(config, file_list, BATCH_SIZE_BYTES)
}

#[cfg(all(test, feature = "ignore"))]
pub(super) fn build_index_with_batch_size(
    config: Config,
    batch_size_bytes: u64,
) -> Result<super::Index, IndexError> {
    let file_list = enumerate_files(&config)?;
    build_index_from_file_list(config, file_list, batch_size_bytes)
}

/// Full build from a caller-supplied file list.
pub(super) fn build_index_from_external_records(
    config: Config,
    records: Vec<ExternalFileRecord>,
) -> Result<super::Index, IndexError> {
    build_index_from_file_list(
        config,
        normalize_external_records(records)?,
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

fn build_index_from_file_list(
    config: Config,
    file_list: Vec<FileRecord>,
    batch_size_bytes: u64,
) -> Result<super::Index, IndexError> {
    fs::create_dir_all(&config.index_dir)?;
    // Security: restrict the index directory to owner-only access. A group- or
    // world-writable index directory allows an unprivileged process to replace
    // segment files or symbols.db between open and mmap, enabling a SIGBUS DoS
    // via ftruncate() racing the xxh64 checksum pass (SIGBUS window) or
    // injecting crafted DB rows (Vuln 4). Mode 0700 eliminates both threats in
    // single-principal deployments. Multi-tenant shared-cache deployments must
    // additionally arrange for mandatory locking or separate index directories.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.index_dir, fs::Permissions::from_mode(0o700))?;
    }

    // Exclusive lock for the duration of the build. Prevents concurrent
    // builds and blocks open() callers until the build completes.
    let lock_file = super::helpers::open_dir_lock_file(&config.index_dir)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    // Full builds and incremental commits both rewrite shared index state,
    // so they must serialize on the same writer lock.
    let write_lock = super::helpers::acquire_writer_lock(&config.index_dir)?;

    // Startup GC: remove orphaned segments left by any previously crashed build.
    // Runs under the exclusive lock, so no readers are active. Safe to ignore a
    // missing or malformed manifest — first builds have neither.
    // Capture the prior calibrated threshold while the manifest is loaded so
    // repeat builds can skip recalibration (see below).
    let mut prev_threshold: Option<f64> = None;
    if let Ok(prev_manifest) = Manifest::load(&config.index_dir) {
        prev_threshold = prev_manifest.scan_threshold_fraction;
        if let Err(e) = prev_manifest.gc_orphan_segments(&config.index_dir) {
            if config.verbose {
                eprintln!("syntext: startup gc: {e}");
            }
        }
    }

    let total_candidate = file_list.len();
    if config.verbose {
        eprintln!("syntext: indexing {} candidate files", total_candidate);
    }

    // Split into ~256MB batches and process each.
    let batches = split_batches(&file_list, batch_size_bytes.max(1));
    let mut seg_refs: Vec<SegmentRef> = Vec::new();
    // Files successfully indexed, in doc_id order: position in this vec
    // equals the file's doc_id (and stable file_id for the symbol index).
    let mut indexed_files: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(total_candidate);
    let mut next_doc_id: u32 = 0;

    for batch in &batches {
        // Security: checked_add guards against u32 overflow in the doc_id
        // space. The overlay path already uses checked_add; this matches it.
        // Parallel: read file content and extract grams.
        // results[i] is None if file i was binary or could not be read.
        let verbose = config.verbose;
        let map_fn = |(abs_path, _, _): &(PathBuf, PathBuf, u64)| -> Option<(u64, Vec<u64>)> {
            // Security: close the TOCTOU window between enumerate_files()'s
            // symlink resolution (symlink_metadata + canonicalize) and this
            // read. open_readonly_nofollow blocks final-component substitution;
            // verify_fd_matches_stat catches directory-component swaps.
            let pre_meta = match std::fs::symlink_metadata(abs_path) {
                Ok(m) => m,
                Err(e) => {
                    if verbose {
                        eprintln!("syntext: skipping {}: stat: {e}", abs_path.display());
                    }
                    return None;
                }
            };
            let mut file = match super::open_readonly_nofollow(abs_path) {
                Ok(f) => f,
                Err(e) => {
                    if verbose {
                        eprintln!("syntext: skipping {}: open: {e}", abs_path.display());
                    }
                    return None;
                }
            };
            #[cfg(any(unix, windows))]
            if !super::verify_fd_matches_stat(&file, &pre_meta) {
                return None;
            }
            #[cfg(not(any(unix, windows)))]
            let _ = &pre_meta;
            let mut raw = Vec::new();
            if let Err(e) = file.read_to_end(&mut raw) {
                if verbose {
                    eprintln!("syntext: skipping {}: read: {e}", abs_path.display());
                }
                return None;
            }
            let content = crate::index::normalize_encoding(&raw, config.verbose);
            if is_binary(&content) {
                return None;
            }
            let hash = xxh64(content.as_ref(), 0);
            Some((hash, build_all(content.as_ref())))
        };
        #[cfg(feature = "rayon")]
        let results: Vec<Option<(u64, Vec<u64>)>> = batch.par_iter().map(map_fn).collect();
        #[cfg(not(feature = "rayon"))]
        let results: Vec<Option<(u64, Vec<u64>)>> = batch.iter().map(map_fn).collect();

        let batch_start_doc_id = next_doc_id;
        let mut writer = SegmentWriter::with_capacity(batch.len(), 120);
        for ((abs_path, rel_path, size), result) in batch.iter().zip(results.iter()) {
            if let Some((content_hash, grams)) = result {
                let doc_id = next_doc_id;
                next_doc_id = next_doc_id
                    .checked_add(1)
                    .ok_or(IndexError::DocIdOverflow {
                        base_doc_count: doc_id,
                        overlay_docs: 0,
                    })?;
                writer.add_document(doc_id, rel_path, *content_hash, *size);
                for &gram_hash in grams {
                    writer.add_gram_posting(gram_hash, doc_id);
                }
                indexed_files.push((abs_path.clone(), rel_path.clone()));
            } else {
                // File was binary or unreadable; logged in map_fn when verbose.
                let _ = abs_path;
            }
        }

        if writer.doc_count() == 0 {
            continue; // Empty batch (all files were binary/unreadable).
        }

        let meta = writer.write_to_dir(&config.index_dir)?;

        // Sanity check: the posting/dictionary overhead should not exceed 50% of
        // the raw content size. Larger ratios indicate an unexpectedly dense gram
        // distribution and may signal a tokenizer or threshold misconfiguration.
        let content_size: u64 = batch
            .iter()
            .zip(results.iter())
            .filter_map(|((_, _, size), r)| r.as_ref().map(|_| size))
            .sum();
        // For v3 segments the segment size is .dict + .post combined.
        let dict_size = fs::metadata(config.index_dir.join(&meta.dict_filename))
            .map(|m| m.len())
            .unwrap_or(0);
        let post_size = fs::metadata(config.index_dir.join(&meta.post_filename))
            .map(|m| m.len())
            .unwrap_or(0);
        let seg_size = dict_size + post_size;
        if config.verbose && seg_size > content_size / 2 && content_size > 0 {
            eprintln!(
                "syntext: warning: segment is {seg_size} bytes for {content_size} bytes content"
            );
        }

        let mut seg_ref: SegmentRef = meta.into();
        seg_ref.base_doc_id = Some(batch_start_doc_id);
        seg_refs.push(seg_ref);
    }

    let total_indexed = next_doc_id;

    // Index-vs-scan crossover threshold. The value is hardware-dependent, not
    // content-dependent, so a repeat build reuses the prior manifest's value
    // instead of re-sampling file reads and re-running the bitmap microbench.
    // The clamp bounds any staleness error; `recalibrate` covers machine moves.
    let scan_threshold = match prev_threshold {
        Some(prev) if !config.recalibrate => {
            let reused = prev.clamp(0.01, 0.50);
            if config.verbose {
                eprintln!("syntext: reusing calibrated scan threshold: {reused:.3}");
            }
            reused
        }
        _ => {
            let scan_paths: Vec<PathBuf> =
                indexed_files.iter().map(|(abs, _)| abs.clone()).collect();
            let calibrated = calibrate_threshold(&scan_paths);
            if config.verbose {
                eprintln!("syntext: calibrated scan threshold: {calibrated:.3}");
            }
            calibrated
        }
    };
    // Write manifest.
    let mut manifest = Manifest::new(seg_refs, total_indexed);
    manifest.base_commit = super::helpers::current_repo_head(&config.repo_root)?;
    manifest.scan_threshold_fraction = Some(scan_threshold);
    manifest.save(&config.index_dir)?;
    // Post-build GC: delete segments from the previous build that are no
    // longer in the new manifest. Distinct from the startup GC above, which
    // only removes segments orphaned by a prior crash (not in any manifest).
    manifest.gc_orphan_segments(&config.index_dir)?;

    if config.verbose {
        eprintln!(
            "syntext: indexed {} files into {} segment(s)",
            total_indexed,
            manifest.segments.len()
        );
    }

    // Build symbol index (T052) — requires `symbols` feature.
    #[cfg(feature = "symbols")]
    {
        let db_path = config.index_dir.join("symbols.db");
        // Remove stale DB from previous builds.
        let _ = fs::remove_file(&db_path);
        match crate::symbol::SymbolIndex::open(&db_path) {
            Ok(sym_idx) => {
                // Iterate in doc_id order; binary/unreadable files were already
                // filtered from indexed_files during gram indexing. file_id =
                // position in indexed_files, which matches the doc_id assigned
                // above. Using unwrap_or(0) on a fallback lookup would collide
                // with the first legitimately indexed file and corrupt its
                // symbol rows and any incremental delete operation.
                for (file_id, (abs_path, rel_path)) in indexed_files.iter().enumerate() {
                    let file_id = file_id as u32;
                    let Ok(raw) = fs::read(abs_path) else {
                        continue;
                    };
                    let content = crate::index::normalize_encoding(&raw, config.verbose);
                    if is_binary(&content) {
                        continue;
                    }
                    let rel_path_str = rel_path.to_string_lossy();
                    if let Err(e) = sym_idx.index_file(file_id, &rel_path_str, content.as_ref()) {
                        if config.verbose {
                            eprintln!(
                                "syntext: warning: symbol index failed for {}: {e}",
                                rel_path.display()
                            );
                        }
                    }
                }
                if config.verbose {
                    eprintln!("syntext: symbol index built");
                }
            }
            Err(e) => {
                if config.verbose {
                    eprintln!("syntext: warning: could not build symbol index: {e}");
                }
            }
        }
    }

    // Downgrade the exclusive directory lock to shared in-place. flock has no
    // atomic EX -> SH downgrade, so there is a brief window between unlock and
    // try_lock_shared. A competing writer could acquire EX during that window,
    // but it will fail at write.lock (still held) and release immediately. If
    // try_lock_shared races with that brief hold, we surface LockConflict and
    // the caller retries. write.lock is dropped only AFTER the shared lock is
    // held to bound the window to a single failed try_lock_exclusive.
    lock_file
        .unlock()
        .map_err(|e| IndexError::CorruptIndex(format!("failed to unlock dir lock: {e}")))?;
    lock_file
        .try_lock_shared()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    drop(write_lock);
    super::Index::open_with_lock(config, lock_file)
}

