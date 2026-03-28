//! Index build pipeline: full-corpus segment construction.
//!
//! `build_index()` is the implementation of `Index::build()`. It is split out
//! to keep `mod.rs` under the 400-line quality gate. Everything here runs only
//! during a fresh `st index` build; it is not used by open/search/commit.

use std::fs;

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
pub(super) fn calibrate_threshold(indexed_paths: &[String], config: &Config) -> f64 {
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
    let sample_paths: Vec<&str> = (0..sample_count)
        .map(|i| indexed_paths[(i * stride).min(total - 1)].as_str())
        .collect();

    let mut docs_read = 0usize;
    let mut scan_elapsed_ns = 0u64;
    for path in &sample_paths {
        let t0 = std::time::Instant::now();
        if std::fs::read(config.repo_root.join(path)).is_ok() {
            docs_read += 1;
            scan_elapsed_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    if docs_read == 0 || scan_elapsed_ns == 0 {
        return DEFAULT;
    }
    let scan_ns_per_doc = scan_elapsed_ns / docs_read as u64;

    // --- Posting cost: synthetic Roaring bitmap AND microbenchmark ---
    // Two bitmaps with BITMAP_ENTRIES entries each, interleaved so the AND
    // result is half-dense (worst-case for the AND algorithm).
    let a: RoaringBitmap = (0..BITMAP_ENTRIES).collect();
    let b: RoaringBitmap = (0..BITMAP_ENTRIES * 2).step_by(2).collect();

    let t1 = std::time::Instant::now();
    for _ in 0..BITMAP_REPS {
        let _ = &a & &b;
    }
    let posting_elapsed_ns = t1.elapsed().as_nanos() as u64;
    // Cost per entry processed by the AND (both bitmaps contribute BITMAP_ENTRIES entries).
    let total_entries_processed = BITMAP_ENTRIES as u64 * BITMAP_REPS as u64 * 2;
    if posting_elapsed_ns == 0 {
        return DEFAULT;
    }
    let posting_ns_per_entry = posting_elapsed_ns / total_entries_processed;

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
    fs::create_dir_all(&config.index_dir)?;

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
    let batches = split_batches(&file_list, BATCH_SIZE_BYTES);
    let mut seg_refs: Vec<SegmentRef> = Vec::new();
    let mut indexed_paths: Vec<String> = Vec::new();
    let mut next_doc_id: u32 = 0;

    for batch in &batches {
        // Security: checked_add guards against u32 overflow in the doc_id
        // space. The overlay path already uses checked_add; this matches it.
        // Parallel: read file content and extract grams.
        // results[i] is None if file i was binary or could not be read.
        let results: Vec<Option<(u64, Vec<u64>)>> = batch
            .par_iter()
            .map(|(abs_path, _, _)| {
                let content = fs::read(abs_path).ok()?;
                if is_binary(&content) {
                    return None;
                }
                let hash = xxh64(&content, 0);
                Some((hash, build_all(&content)))
            })
            .collect();

        let mut writer = SegmentWriter::with_capacity(batch.len(), 120);
        for ((abs_path, rel_path, size), result) in batch.iter().zip(results.iter()) {
            if let Some((content_hash, grams)) = result {
                let doc_id = next_doc_id;
                next_doc_id = next_doc_id.checked_add(1).ok_or(
                    IndexError::DocIdOverflow {
                        base_doc_count: doc_id,
                        overlay_docs: 0,
                    },
                )?;
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

        seg_refs.push(meta.into());
    }

    let total_indexed = next_doc_id;

    // Calibrate index-vs-scan crossover threshold from actual disk timing.
    let scan_threshold = calibrate_threshold(&indexed_paths, &config);
    if config.verbose {
        eprintln!("syntext: calibrated scan threshold: {:.3}", scan_threshold);
    }
    // Write manifest.
    let mut manifest = Manifest::new(seg_refs, total_indexed);
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
                        if let Ok(content) = fs::read(abs_path) {
                            if !is_binary(&content) {
                                // file_id from path_index built in open(); use position
                                // in indexed_paths as a stable id for build time.
                                let file_id = indexed_paths
                                    .iter()
                                    .position(|p| p == rel_path)
                                    .unwrap_or(0)
                                    as u32;
                                if let Err(e) = sym_idx.index_file(file_id, rel_path, &content) {
                                    if config.verbose {
                                        eprintln!(
                                            "syntext: warning: symbol index failed for {rel_path}: {e}"
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

    // Drop exclusive lock before open() acquires shared lock.
    // Another process can grab exclusive in the gap; retry with backoff.
    drop(write_lock);
    drop(lock_file);
    // Retry open() if a competing process grabbed the exclusive lock in the gap
    // between our drop and open()'s try_lock_shared.
    let mut delay = std::time::Duration::from_millis(10);
    for _ in 0..4u32 {
        match super::Index::open(config.clone()) {
            Err(IndexError::LockConflict(_)) => {
                std::thread::sleep(delay);
                delay = delay.saturating_mul(2);
            }
            result => return result,
        }
    }
    super::Index::open(config)
}
