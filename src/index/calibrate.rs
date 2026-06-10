//! Index-vs-scan crossover calibration.
//!
//! Extracted from `build.rs` to keep that file under the 400-line quality
//! gate. Runs only during a full build, and only when no prior manifest
//! threshold can be reused (or `Config::recalibrate` is set).

use std::path::{Path, PathBuf};

use roaring::RoaringBitmap;

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
pub(super) fn calibrate_threshold(indexed_paths: &[PathBuf]) -> f64 {
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
        let _ = std::fs::read(path);
    }

    let mut docs_read = 0usize;
    // Use u128 to match as_nanos() return type; avoids silent truncation on
    // platforms where accumulated sample time would overflow u64 (~584 years
    // of nanoseconds). Cast to u64 only after the per-doc division.
    let mut scan_elapsed_ns = 0u128;
    for path in &sample_paths {
        let t0 = std::time::Instant::now();
        if std::fs::read(path).is_ok() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn calibrate_threshold_empty_paths_returns_default() {
        // B08: calibrate_threshold must not divide by zero (or panic) when
        // indexed_paths is empty. An empty repository returns the default
        // threshold (0.10) instead of attempting sample_count / 0.
        let threshold = calibrate_threshold(&[]);
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
        let mut absolute_paths = Vec::new();
        for i in 0..5 {
            let abs = repo.path().join(format!("f{i}.rs"));
            std::fs::write(&abs, format!("fn test_{i}() {{}}\n")).unwrap();
            absolute_paths.push(abs);
        }
        let threshold = calibrate_threshold(&absolute_paths);
        assert!(
            (0.01..=0.50).contains(&threshold),
            "threshold {threshold} outside [0.01, 0.50]"
        );
    }
}
