//! Index build pipeline: full-corpus segment construction.
//!
//! `build_index()` is the implementation of `Index::build()`. It is split out
//! to keep `mod.rs` under the 400-line quality gate. Everything here runs only
//! during a fresh `st index` build; it is not used by open/search/commit.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(feature = "fs2")]
use fs2::FileExt;
#[cfg(feature = "rayon")]
use rayon::prelude::*;
use roaring::RoaringBitmap;
use xxhash_rust::xxh64::xxh64;

use crate::index::manifest::{Manifest, SegmentRef};
use crate::index::segment::SegmentWriter;
use crate::index::walk::is_binary;
#[cfg(feature = "ignore")]
use crate::index::walk::{enumerate_files, split_batches};
use crate::tokenizer::build_all;
use crate::{Config, IndexError};

/// Target batch size (content bytes) before flushing a segment.
pub(super) const BATCH_SIZE_BYTES: u64 = 256 * 1024 * 1024;

/// Measure the crossover fraction where index lookup becomes cheaper than a
/// full scan for this repository.
///
/// Returns a value in [0.01, 0.50]. Falls back to 0.10 if measurement fails
/// (e.g., no files indexed, timing resolution too coarse).
///
/// # Why sequential reads are acceptable here
///
/// The scan cost measurement reads files sequentially. Parallel I/O on NVMe
/// (high queue depth) would lower effective scan cost per doc, and that speedup
/// does NOT cancel in the ratio: posting cost is pure CPU (roaring bitmap AND),
/// so `threshold = scan / (scan + posting)` shifts downward when scan shrinks
/// but posting stays fixed. The calibrated threshold is therefore slightly
/// higher than the true parallel-aware threshold, biasing toward index use.
///
/// This bias is acceptable for three reasons:
///
/// 1. The warmup pass populates the page cache. The timed pass measures
///    hot-cache memcpy cost, not device latency. Parallel I/O gains come
///    primarily from device queue depth on cold reads; for cached files the
///    speedup factor is small.
/// 2. The bias direction is conservative: we use the index slightly more than
///    optimal, paying marginal extra posting cost but reading fewer files.
///    This never produces wrong results, only marginally suboptimal routing.
/// 3. The clamp to [0.01, 0.50] bounds the maximum error regardless of
///    measurement quality.
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

    // Warm the page cache first so we do not calibrate against first-read
    // latency right after a clone or reboot.
    for path in &sample_paths {
        let _ = std::fs::read(config.repo_root.join(path));
    }

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
#[cfg(feature = "ignore")]
pub(super) fn build_index(config: Config) -> Result<super::Index, IndexError> {
    build_index_with_batch_size(config, BATCH_SIZE_BYTES)
}

#[cfg(feature = "ignore")]
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
    let write_lock = super::helpers::acquire_writer_lock(&config.index_dir)?;

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
                indexed_paths.push(rel_path.clone());
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

    // Calibrate index-vs-scan crossover threshold from actual disk timing.
    let scan_threshold = calibrate_threshold(&indexed_paths, &config);
    if config.verbose {
        eprintln!("syntext: calibrated scan threshold: {:.3}", scan_threshold);
    }
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
                // Re-enumerate: iterate batches and index each file's symbols.
                for batch in &batches {
                    for (abs_path, rel_path, _size) in batch {
                        if let Ok(raw) = fs::read(abs_path) {
                            let content = crate::index::normalize_encoding(&raw, config.verbose);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use tempfile::TempDir;

    #[test]
    fn calibrate_threshold_empty_paths_returns_default() {
        // B08: calibrate_threshold must not divide by zero (or panic) when
        // indexed_paths is empty. An empty repository returns the default
        // threshold (0.10) instead of attempting sample_count / 0.
        let index_dir = TempDir::new().unwrap();
        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: index_dir.path().to_path_buf(),
            ..Config::default()
        };
        let threshold = calibrate_threshold(&[], &config);
        assert_eq!(
            threshold, 0.10,
            "empty path list must return default threshold 0.10, got {threshold}"
        );
    }

    /// calibrate_threshold always returns a value in [0.01, 0.50] when given
    /// real files to sample. This documents the clamp invariant regardless of
    /// disk speed or timing resolution.
    #[test]
    fn calibrate_threshold_returns_clamped_value() {
        let repo = TempDir::new().unwrap();
        let mut paths = Vec::new();
        for i in 0..5 {
            let rel = PathBuf::from(format!("f{i}.rs"));
            std::fs::write(repo.path().join(&rel), format!("fn test_{i}() {{}}\n")).unwrap();
            paths.push(rel);
        }
        let config = Config {
            repo_root: repo.path().to_path_buf(),
            index_dir: repo.path().join(".syntext"),
            ..Config::default()
        };
        let threshold = calibrate_threshold(&paths, &config);
        assert!(
            (0.01..=0.50).contains(&threshold),
            "threshold {threshold} outside [0.01, 0.50]"
        );
    }
}
