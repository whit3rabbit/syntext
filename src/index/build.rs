//! Index build pipeline: full-corpus segment construction.
//!
//! `build_index()` is the implementation of `Index::build()`. It is split out
//! to keep `mod.rs` under the 400-line quality gate. Everything here runs only
//! during a fresh `st index` build; it is not used by open/search/commit.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use rayon::prelude::*;
use roaring::RoaringBitmap;
use xxhash_rust::xxh64::xxh64;

use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::segment::SegmentWriter;
use crate::index::walk::{enumerate_files, is_binary, split_batches};
use crate::tokenizer::build_all;
use crate::{Config, IndexError};

/// Target batch size (content bytes) before flushing a segment.
pub(super) const BATCH_SIZE_BYTES: u64 = 256 * 1024 * 1024;

/// Measure the crossover fraction where index lookup becomes cheaper than a
/// full scan for this repository.
///
/// Returns a value in [0.01, 0.50]. Falls back to 0.10 if measurement fails
/// (e.g., no files indexed, timing resolution too coarse).
pub(super) fn calibrate_threshold(indexed_paths: &[PathBuf], config: &Config) -> f64 {
    const DEFAULT: f64 = 0.10;
    const SCAN_SAMPLE: usize = 100;
    // Entries per bitmap in the posting-cost microbenchmark.
    const BITMAP_ENTRIES: u32 = 10_000;
    // Higher reps produce more stable calibrations on loaded systems.
    const BITMAP_REPS: u32 = 100;

    let total = indexed_paths.len();
    if total == 0 {
        return DEFAULT;
    }

    // --- Scan cost: time reading a strided sample of indexed files ---
    let sample_count = SCAN_SAMPLE.min(total);
    // Use a stride so we sample evenly across the corpus (sorted by path).
    let stride = (total / sample_count).max(1);
    let sample_paths: Vec<&Path> = (0..sample_count)
        .map(|i| indexed_paths[(i * stride).min(total - 1)].as_path())
        .collect();

    let mut docs_read = 0usize;
    // Use u128 to match as_nanos() return type; avoids silent truncation on
    // platforms where accumulated sample time would overflow u64 (~584 years
    // of nanoseconds). Cast to u64 only after the per-doc division.
    let mut scan_elapsed_ns = 0u128;
    for path in &sample_paths {
        let t0 = std::time::Instant::now();
        if std::fs::read(config.repo_root.join(path)).is_ok() {
            docs_read += 1;
            scan_elapsed_ns += t0.elapsed().as_nanos();
        }
    }

    if docs_read == 0 || scan_elapsed_ns == 0 {
        return DEFAULT;
    }
    // Per-doc value is always << u64::MAX; cast is safe after division.
    let scan_ns_per_doc = (scan_elapsed_ns / docs_read as u128) as u64;

    // --- Posting cost: synthetic Roaring bitmap AND microbenchmark ---
    // Two bitmaps with BITMAP_ENTRIES entries each, interleaved so the AND
    // result is half-dense (worst-case for the AND algorithm).
    let a: RoaringBitmap = (0..BITMAP_ENTRIES).collect();
    let b: RoaringBitmap = (0..BITMAP_ENTRIES * 2).step_by(2).collect();

    let t1 = std::time::Instant::now();
    for _ in 0..BITMAP_REPS {
        let _ = &a & &b;
    }
    // u128 accumulation; per-entry value cast to u64 after division.
    let posting_elapsed_ns = t1.elapsed().as_nanos();
    // Cost per entry processed by the AND (both bitmaps contribute BITMAP_ENTRIES entries).
    let total_entries_processed = BITMAP_ENTRIES as u64 * BITMAP_REPS as u64 * 2;
    if posting_elapsed_ns == 0 {
        return DEFAULT;
    }
    let posting_ns_per_entry = (posting_elapsed_ns / total_entries_processed as u128) as u64;

    if posting_ns_per_entry == 0 {
        // Posting decode is immeasurably fast relative to scan — use index
        // aggressively, but stay within safe upper bound.
        return 0.50;
    }

    // Crossover fraction: use the index when candidates/total_docs < threshold.
    //
    // Cost(index path)  ≈ cardinality * (posting_ns_per_entry + scan_ns_per_doc)
    // Cost(full scan)   ≈ total_docs  * scan_ns_per_doc
    //
    // Equating the two:
    //   threshold = scan_ns_per_doc / (scan_ns_per_doc + posting_ns_per_entry)
    let threshold = scan_ns_per_doc as f64 / (scan_ns_per_doc + posting_ns_per_entry) as f64;
    threshold.clamp(0.01, 0.50)
}

/// Full-corpus index build. Called by `Index::build()`; returns a ready-to-use
/// `Index` by delegating to `Index::open()` after writing all segments.
pub(super) fn build_index(config: Config) -> Result<super::Index, IndexError> {
    build_index_with_batch_size(config, BATCH_SIZE_BYTES)
}

pub(super) fn build_index_with_batch_size(
    config: Config,
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
    let lock_path = config.index_dir.join("lock");
    let lock_file = std::fs::File::create(&lock_path)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    // Full builds and incremental commits both rewrite shared index state,
    // so they must serialize on the same writer lock.
    let write_lock = super::acquire_writer_lock(&config.index_dir)?;

    // Startup GC: remove orphaned segments left by any previously crashed build.
    // Runs under the exclusive lock, so no readers are active. Safe to ignore a
    // missing or malformed manifest — first builds have neither.
    if let Ok(prev_manifest) = Manifest::load(&config.index_dir) {
        if let Err(e) = prev_manifest.gc_orphan_segments(&config.index_dir) {
            if config.verbose {
                eprintln!("syntext: startup gc: {e}");
            }
        }
    }

    // Enumerate all candidate files, sorted by relative path.
    let file_list = enumerate_files(&config)?;
    let total_candidate = file_list.len();
    if config.verbose {
        eprintln!("syntext: indexing {} candidate files", total_candidate);
    }

    // Split into ~256MB batches and process each.
    let batches = split_batches(&file_list, batch_size_bytes.max(1));
    let mut seg_refs: Vec<SegmentRef> = Vec::new();
    let mut indexed_paths: Vec<PathBuf> = Vec::new();
    let mut next_doc_id: u32 = 0;

    for batch in &batches {
        // Security: checked_add guards against u32 overflow in the doc_id
        // space. The overlay path already uses checked_add; this matches it.
        // Parallel: read file content and extract grams.
        // results[i] is None if file i was binary or could not be read.
        let results: Vec<Option<(u64, Vec<u64>)>> = batch
            .par_iter()
            .map(|(abs_path, _, _)| {
                // Security: close the TOCTOU window between enumerate_files()'s
                // symlink resolution (symlink_metadata + canonicalize) and this
                // read. A concurrent rename() could swap the canonical target
                // after the walk's stat. open_readonly_nofollow blocks final-
                // component symlink substitution (O_NOFOLLOW); verify_fd_matches_stat
                // catches directory-component swaps by comparing dev/ino before
                // open vs after open. This matches the same pattern used in
                // commit_batch and the resolver hot path.
                let pre_meta = std::fs::symlink_metadata(abs_path).ok()?;
                let mut file = super::open_readonly_nofollow(abs_path).ok()?;
                if !super::verify_fd_matches_stat(&file, &pre_meta) {
                    return None;
                }
                let mut raw = Vec::new();
                file.read_to_end(&mut raw).ok()?;
                let content = crate::index::normalize_encoding(&raw);
                if is_binary(&content) {
                    return None;
                }
                let hash = xxh64(content.as_ref(), 0);
                Some((hash, build_all(content.as_ref())))
            })
            .collect();

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
                indexed_paths.push(rel_path.clone());
            } else {
                // File was binary or unreadable; log at trace level if verbose.
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

    // Calibrate index-vs-scan crossover threshold from actual disk timing.
    let scan_threshold = calibrate_threshold(&indexed_paths, &config);
    if config.verbose {
        eprintln!("syntext: calibrated scan threshold: {:.3}", scan_threshold);
    }
    // Write manifest.
    let mut manifest = Manifest::new(seg_refs, total_indexed);
    manifest.base_commit = super::current_repo_head(&config.repo_root)?;
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
                // Re-enumerate: iterate batches and index each file's symbols.
                for batch in &batches {
                    for (abs_path, rel_path, _size) in batch {
                        if let Ok(raw) = fs::read(abs_path) {
                            let content = crate::index::normalize_encoding(&raw);
                            if !is_binary(&content) {
                                // file_id from path_index built in open(); use position
                                // in indexed_paths as a stable id for build time.
                                // Security: skip symbol indexing for files absent from
                                // indexed_paths (binary or unreadable during gram indexing).
                                // unwrap_or(0) would silently assign file_id 0, colliding
                                // with the first legitimately indexed file and corrupting
                                // its symbol rows and any incremental delete operation.
                                let Some(pos) = indexed_paths.iter().position(|p| p == rel_path)
                                else {
                                    continue;
                                };
                                let file_id = pos as u32;
                                let rel_path_str = rel_path.to_string_lossy();
                                if let Err(e) =
                                    sym_idx.index_file(file_id, &rel_path_str, content.as_ref())
                                {
                                    if config.verbose {
                                        eprintln!(
                                            "syntext: warning: symbol index failed for {}: {e}",
                                            rel_path.display()
                                        );
                                    }
                                }
                            }
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

    // Downgrade the exclusive directory lock to shared in-place.
    // This ensures no competing build can start between unlock and re-lock.
    lock_file
        .unlock()
        .map_err(|e| IndexError::CorruptIndex(format!("failed to unlock dir lock: {e}")))?;
    lock_file
        .try_lock_shared()
        .map_err(|_| IndexError::LockConflict(config.index_dir.clone()))?;
    // Drop writer lock only after the shared lock is held, closing the gap.
    drop(write_lock);
    super::Index::open_with_lock(config, lock_file)
}
