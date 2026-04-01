# Changelog

All notable changes to this project will be documented in this file.

## [1.0.2] - 2026-03-31

### Fixed

- Replace `io::Error::other` with `io::Error::new(io::ErrorKind::Other, ...)`
  in manifest.rs for Rust < 1.74 compatibility (3 call sites).
- Add verbose-gated stderr logging for file read failures in build pipeline.
  Previously, permission errors and read failures were silently swallowed.
- Expand `verify_fd_matches_stat` TOCTOU check to Windows (was unix-only).

### Changed

- Document `calibrate_threshold` sequential-read bias and why it is acceptable.

## [1.0.1] - 2026-03-31

### Fixed

- Windows: normalize paths to forward slashes at ingestion boundaries to fix
  path matching on Windows builds.
- Windows: gate `sync_all` on directory handles behind `#[cfg(not(windows))]`
  to avoid `Access is denied` errors.
- Windows: use `io::Error::new(io::ErrorKind::Other, ...)` instead of
  `io::Error::other` for Rust < 1.74 compatibility.
- CI: address Windows CI failures (OS error 5, concurrent file handle locks).

## [1.0.0] - 2026-03-29

### Added

- Full index build from repository files (sparse n-gram tokenizer, batched segments, SNTX v3 format)
- Literal and regex search with ripgrep-validated correctness
- Incremental overlay updates with batch commit and ArcSwap snapshot isolation
- Path/type scoping via Roaring bitmap component index
- CLI (`st`) with grep-compatible output, NDJSON, context lines, heading mode, invert match
- Encoding normalization (UTF-8 BOM stripping, UTF-16 LE/BE transcoding)
- Compaction (selective segment rewrite from snapshot)
- Calibrated scan threshold (index-vs-scan crossover measured at build time)
- Symbol extraction behind `--features symbols` (Tree-sitter + SQLite)
- Advisory file locking for concurrent index access
- Benchmark harness (`scripts/bench_compare.py`) with preset catalog
- Pre-trained bigram weight table from 498 GB corpus (13 languages)
- Early exit for `--max-count` via atomic counter across rayon tasks

### Fixed

- `base_doc_id_limit` overflow now returns error instead of silently dropping segments (B01)
- `varint_encode` rejects duplicate doc_ids with strict `<` check (B02)
- V2 posting offset validates against actual postings section start (B03)
- Overlapping base_doc_id ranges rejected on index open (B04)
- `build_incremental` uses saturating arithmetic to prevent underflow (B05)
- `commit_batch` uses `saturating_add(1)` for max_file_size sentinel (B06)
- `**/word` glob patterns use component-boundary matching (B07)
- `calibrate_threshold` handles empty repositories without panic (B08)
- `projected_overlay_doc_count` excludes removed_paths from visible_changed (B10)
- Truncated UTF-16 files (odd byte count) produce warnings (B11)
- `cmd_update` handles per-file errors without aborting entire batch (B12)
- `commit_batch` treats NotFound as deletion for TOCTOU safety (B12)

### Performance

- Avoid RoaringBitmap clone in `should_use_index` hot path (B09)
- Atomic early-exit counter for `--max-count` parallel search (B16)
- Eliminate Vec clone in `boundary_positions_lower` via callback pattern (B18)
- Deduplicate symlinked directory targets in walk (B14)

### Security

- O_NOFOLLOW and inode verification on file opens
- Path traversal rejection in search resolver and git stdout
- MAP_PRIVATE mmap to isolate from concurrent writes
- Advisory locking on index directory
- Directory permission enforcement (reject group/other bits)
- Symlink escape prevention with repo root boundary check
- Symlink walk depth capped at 256 sub-walks (B14)
- NFA/DFA size caps to prevent ReDoS
- Segment reader offset hardening
- Git binary resolved to absolute path to prevent PATH hijacking
- Max file size clamped to 1 GB ceiling

### Known Limitations

1. Overlay state is lost on unclean shutdown. Run `st update` or `st index` after a crash.
2. `st -v` (invert match) inverts within candidate files only, not the full corpus.
3. Non-aligned substring queries have ~16% false-negative rate. Token-aligned queries (identifiers, keywords) have 0% false negatives.
4. Index directory must be on local filesystem. NFS/SMB behavior is undefined.
5. Case-insensitive queries produce ~15-20% more candidates due to lowercase normalization. Correct results guaranteed by verifier.
6. `\r`-only line endings (classic Mac) are treated as a single line (matches ripgrep behavior).
7. Symbol search Tier 3 (heuristic) results are approximate. Tree-sitter failures fall back silently.
