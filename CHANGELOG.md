# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- **FABLE EDITION** These were recommendations made by Claude Fable 5 for security/bugs/improvments.
- Durable incremental HEAD-move updates via LSM-style delta segments when the base commit is behind HEAD.
- Checksummed delete-set sidecar (`deletes-<uuid>.idx`) for tracking deleted base documents across restarts, designed to fail closed on corruption to prevent duplicate matching.
- Automatic bounded update-on-search capability with async catch-up updates and staleness warnings.
- Explicit `globset` and `windows-sys` dependencies.
- Git-hooks vendor installer/uninstaller supporting automatic post-commit/checkout/merge/rewrite indexing.
- Custom component-wise raw byte path comparison (`path_util::cmp_path_bytes`) reproducing `Path::cmp` component-wise order exactly without the `Components` iterator overhead.
- Compaction trigger (`FileIdBloat`) firing when `next_file_id` runs 4x ahead of the live path count to prune path tombstones.
- Batched query execution for reference searches (`--refs`), matching definitions against alternations to perform a single-pass regex search instead of sequential full scans.
- Differential testing framework against `ripgrep` (`oracle_self`, `oracle_cli`, `oracle_incremental`).
- Performance benchmarks for index freshness (`bench_freshness`) and large-repository e2e searches (`open_search_e2e`).

### Changed
- Refactored CLI arguments (`args/`) and query scopes (`scope/`) into modular sub-modules.
- Lowered the environment-override `MAX_FILE_SIZE_CEILING` from 1 GiB to 512 MiB.
- Cached gram and query cardinality calculations in `executor` to avoid $O(n \log n)$ evaluations during intersection sorting.
- Replaced the overlay posting-bitmap cache clear-all behavior with a FIFO eviction policy under a 256MB byte-budget.
- Optimized token boundaries and Covering gram extraction.
- Overlay `gram_index` posting lists are now `Arc`-shared to support zero-copy clones and copy-on-write modifications.
- `--column` now compiles the output regex once and reuses it to count matches for the long-line placeholder, keeping the count exact without recompiling per long line.

### Performance
- `Index::open` reads each segment's whole doc-table region in a single positional read (`MmapSegment::iter_docs`) instead of three `pread`s per document (~3x faster open; paid on every open, which bounded update-on-search makes frequent).
- `PathIndex` interns each unique path once as a shared `Arc<Path>` across its three internal maps instead of storing it three times (~5% lower search RSS at 40k files, ~7 MB at 100k).
- Search skips the per-match `line_content` copy for `-l`/`-L` (files-with/without-match) output modes, which never render line bodies.
- Render reuses the encoding-normalized bytes captured during search (`matched_file_bytes`) instead of re-reading and re-normalizing the file on output.

### Security
- Render-time file reads open guaranteed-beneath the repo root (`openat2(RESOLVE_BENEATH)` on Linux, else canonicalize + `O_NOFOLLOW` + fd-verify), closing the symlink-swap TOCTOU window between index time and render time.

### Fixed
- Throttled async catch-up spawning with a coarse TTL stamp so a burst of concurrent stale searches collapses to roughly one `st update` per window instead of stampeding the writer lock.
- Truncated UTF-16 files (odd byte count after the BOM) now decode the incomplete trailing code unit as U+FFFD instead of dropping it, matching ripgrep and removing an `-x` false-positive divergence (oracle fixture `repro_e1c1603c26349124`).
- `-x` (line-regexp) now matches CRLF mode like `rg --crlf`: a trailing `\r` at end-of-line is treated as part of the terminator, so `^pat$` matches a final line `pat\r` and submatch extraction stays consistent with the match decision (oracle fixture `repro_e1477df13c5a98f4`).
- Resolved a predictable temporary file name TOCTOU vulnerability in `write_atomic` by using random UUIDs.
- Canonicalized directory paths before performing sensitive prefix checks in `validate_index_dir`.
- Structured verifier to count backward line-starts relative to a watermark to remove the quadratic $O(\text{matches} \times \text{file\_size})$ cost.
- Divert `--files-without-match` before the empty-results short-circuit and respect `-q` flag.
- Deduplicated `requeue_uncommitted` paths in `PendingEdits` to bound memory growth.
- Rejected literal and escaped newlines (`\n`, `\x0a`, etc.) in query patterns during routing.
- Warn on post-delta update errors instead of failing since the HEAD move is already durable.
- Suppressed confusing "no changes detected" output when a delta segment update successfully runs.

## [1.4.0] - 2026-06-13

### Added

- Opt-in ripgrep/grep fallback for searches against an un-indexed path. Enable
  with `--fallback` or `SYNTEXT_FALLBACK_RG=1`; `st` then runs `ripgrep`
  (preferred) or `grep` (last resort) instead of erroring when no index exists.
  Triggers only on a missing index; a corrupt index or lock conflict still
  fails. ripgrep receives the original arguments unchanged (identical output);
  grep is best-effort and drops output-only modes it cannot produce. See README,
  "Fallback to ripgrep/grep".

## [1.2.0] - 2026-06-06

### Added

- Native features split: introduced `cli` and `native` Cargo feature flags to allow building the library without CLI-specific dependencies (such as `clap`). The `st` binary now requires the `cli` feature.

### Changed

- Switched conditional compilation gates from checking the `clap` feature to the new `cli` feature.
- Improved integration test robustness for advisory file locking (`flock`) on macOS by matching production file open options.
- Added `commit_batch_result` retry helper in tests to safely handle transient lock conflict results.
- Isolated the Cursor protocol test to a temporary directory to avoid workspace pollution.

## [1.1.0] - 2026-04-25

### Added

- Native multi-harness agent hooks for Claude Code, Cursor, GitHub Copilot,
  Gemini CLI, OpenCode, OpenClaw, Codex CLI, Cline / Roo Code, Windsurf,
  Kilo Code, and Google Antigravity.
- RTK-style `st init` installer shortcuts plus explicit
  `st agent install|show|uninstall` commands.
- Conservative `rg` / `grep` rewrite path that only rewrites safe agent shell
  searches when `.syntext/` exists.

### Changed

- README now documents agent harness install locations and supported scopes.
- `install.sh` default version updated to 1.1.0.

## [1.0.2] - 2026-03-31

### Fixed

- Replace `io::Error::other` with `io::Error::new(io::ErrorKind::Other, ...)`
  in manifest.rs for Rust < 1.74 compatibility (3 call sites).
- Add verbose-gated stderr logging for file read failures in build pipeline.
  Previously, permission errors and read failures were silently swallowed.
- Windows: stub `verify_fd_matches_stat` to avoid unstable `windows_by_handle`
  feature (`file_index()`, `volume_serial_number()`). Degrades to no-op until
  rust-lang/rust#63010 stabilizes.

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
