# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- **Swift bindings** (`ffi` Cargo feature + `swift/` package): a hand-written C ABI over `Index` and a new mutable in-memory document index, shipped as the `SyntextFFI.xcframework` SPM binary target (macOS, universal arm64+x86_64). Two APIs: `SyntextIndex` (build/open/search/searchFresh/notify/commit/updateFromGit/verify over a project directory) and `SyntextChatIndex` (add/remove/commit/search over caller-supplied in-memory documents, for chat-style content). Results cross as JSON; every match carries the exact line bytes base64-encoded alongside the lossy display string because submatch offsets index the original bytes. Panics are caught at the boundary (`catch_unwind`); errors cross as stable append-only codes (`SYNTEXT_ERR_*`, LockConflict retryable). New `src/ffi/` (`mod/dto/index/mem`), `src/index/mem_index.rs` (`MemIndex`: RwLock doc map + ArcSwap snapshot, traversal-shaped ids rejected by the same guard the wasm index uses), and shared snapshot construction extracted into `build_overlay_snapshot` (now compiled under `ffi` as well as `wasm`, with an empty-path rejection added to `validate_doc_id`). Tests: `tests/integration/ffi.rs` (`cargo test --features ffi`) and `swift/Tests` (`swift test` after `swift/Scripts/build-xcframework.sh`). CI: `test-ffi` and `test-swift` jobs; releases build and publish the xcframework zip plus an automated `update-swift-package` pin job. See docs/SWIFT.md.

### Fixed
- Lock acquisition no longer collapses every `flock(2)` failure into "index locked by another process". `File::try_lock`'s `WouldBlock` (a real competing holder) is now told apart from an I/O failure (`EINTR`, or `ENOLCK` when the kernel lock table is exhausted under heavy process churn): `EINTR` is retried in place, and any other error is logged with its errno before surfacing as a retryable `LockConflict`. Previously a transient kernel error was indistinguishable from contention, which made the macOS nightly's `oracle_incremental::golden_incremental_grow_past_limit` failure (a `commit_batch` on a private, freshly built index dir reporting "locked by another process") undiagnosable.
- `InMemoryIndex`/`MemIndex` no longer re-widen BOM-stripped content back to its raw bytes on the zero-copy fast path in `build_overlay_snapshot`, which was silently re-including the 3-byte BOM in indexed content and match offsets.
- `validate_doc_id` (wasm/ffi in-memory indexes) now rejects degenerate-separator ids (`"chats/1/"`, `"chats//1"`, `"chats/./1"`, a bare `"."`): `Path` equality normalizes these away, so they previously aliased distinct documents onto one path-index entry.
- `syntext_index_free`/`syntext_mem_index_free`/`syntext_error_free` (ffi feature) now run their drop inside the documented panic-firewall (`catch_unwind`), matching every other FFI entry point.
- Swift `CZString` now rejects strings containing an embedded NUL byte instead of silently truncating them at the FFI boundary (`CStr::from_ptr` on the Rust side stops at the first NUL).
- `wasm` and `ffi` Cargo features are now mutually exclusive at compile time (`compile_error!`) instead of failing with a confusing cascade of native-dependency errors on a wasm32 target.
- Corrected `docs/SWIFT.md`, `syntext.h`, and FFI doc comments that overstated `notifyChange`/`notifyDelete` as accepting a repo-relative path (only an absolute path under the repo root actually resolves), and the documented `max_results`/`searchFresh` git-failure semantics to match actual behavior.

### Changed
- `update-swift-package` (release workflow) now checks out `main` explicitly instead of the triggering tag's detached HEAD, and retries the pin-commit push with a rebase, fixing a non-fast-forward failure whenever `main` had advanced past the release tag.
- `test-ffi` (CI) now runs on both `ubuntu-latest` and `macos-14` instead of Linux only, so the ffi Rust tests are exercised on the platform the xcframework actually ships for.

## [2.1.0] - 2026-08-27

### Added
- **Stdin filter mode** (rg parity): `cmd | st 'pat'` and `st 'pat' -` now search the piped/redirected stream in-memory, with rg-compatible output (no filename prefix by default, `<stdin>` label under `-H`/`-l`/`--json`, bare `-c` counts) and exit codes. Implicit-stdin detection is conservative: only a pipe (FIFO) or regular-file redirect engages it; a tty, socket, or `/dev/null` still searches the repo index, so agent shells that attach `/dev/null` to stdin are unaffected. Works without any `.syntext` index. `-v` inverts per-line on a stream (corpus-wide `-v` is meaningless for stdin). `-` mixed with other paths searches **both**: the stdin half is collected before the index opens, `-` is stripped from the path arguments, and the merged output orders the stdin run by the argv position of `-` (exit 0 if either side matches). Exceptions: `-v` mixed still exits 2 (the two invert semantics cannot merge), and a mixed search with no index errors instead of falling back (stdin is already consumed; the fallback child would search an empty stream). New module `src/cli/stdin_search.rs`; shared render/exit dispatch extracted into `search::render_results`.
- `--exclude-dir DIR` (grep compatibility): mapped to negated globs covering the directory at any depth.
- Subcommand-collision hint: when a clap `unexpected argument` error was caused by a pattern word matching a subcommand name (`st -F 'index' -n` routes to `st index`), stderr now suggests `st -e <word>` or `st -- <word>`.
- `SYNTEXT_QUIET_FALLBACK=1` env var to silence the per-search fallback notice (same effect as `-q`, without suppressing match output).
- `-f/--file PATTERNFILE` (rg parity): patterns read one per line and OR-combined with any `-e` patterns (interior empty lines are always-matching empty patterns; trailing newline is a terminator; `-F` escapes each alternative; empty file exits 1 silently like rg; unreadable file exits 2). Previously a hard exit-2 no-op.
- `--rust` / `--rs`: select Rust source files only (equivalent to `-t rs`; a grep-ism seen in mined agent logs).
- Fixed a pre-existing phantom trailing empty line: a zero-width regex match at end-of-content (e.g. an empty `-e ''`/`-f` pattern line, or `x|`) after a final newline used to render as an extra empty numbered line; rg prints none.
- **Rendered lines keep a CRLF line's trailing `\r`** (byte-identical to rg output, all modes: flat, heading, vimgrep, only-matching, context, invert, JSON `lines` text). Matching still runs against the `\r`-stripped line, so which lines match is unchanged; only patterns that would match the `\r` byte itself report different submatch spans (divergence #15 narrowed). The stdin oracle comparisons are now byte-exact instead of `\r`-normalized.
- **Binary (NUL-containing) stdin now follows rg semantics** instead of exiting 1 silently: with a match, every line-printing mode emits exactly `binary file matches (found "\0" byte around offset N)` (filename prefix per mode rules) and exits 0; `-c`/`-l`/`-q`/`--json` keep their normal output (with NUL treated as a line terminator for counts and line numbers, like rg); no match stays silent with exit 1; under `-v` the notice always wins. Residual divergences, documented in #16: binary repo files stay unindexed, and when the first NUL lands beyond rg's first read chunk (~8KB) rg prints preceding matches before the notice while `st` prints only the notice.
- **`--byte-offset` now prints in rg's field order**: last among the prefix fields, immediately before the content (`[path:][line:][col:]byte:content`), instead of leading; context lines keep the `-` separator after it. Exposed by the `--column` un-filtering in the stdin proptest.
- On binary streams, rg's searcher treats NUL as a line terminator: `-c` counts and `--json` line numbers now split at NUL bytes, matching rg.

### Fixed
- JSON submatch enumeration (`--json`) no longer reports a spurious zero-width submatch on a line's isolated slice: a trailing `\r` is stripped before matching again (the oracle's reference `rg` always runs with `--crlf`, which folds it into the line terminator; regressed by the CRLF-passthrough rendering change above), and a zero-width match exactly at the end of a truly unterminated final line (no trailing `\n` anywhere in the file) is excluded, matching rg's line-oriented searcher (oracle fixtures `repro_45977b47dc1f41aa`, re-minimized `repro_8ffb628246265813`).

### Behavior changes
- **`cmd | st 'pat'` previously ignored stdin and silently searched the whole repo index** (exit 0 with wrong results); it now filters the stream. Scripts relying on the old (incorrect) behavior must pass an explicit path argument.
- `st 'pat' -` previously matched nothing (the `-` was a literal path filter, exit 1); it now reads stdin.
- `--column` now forces line numbers on when neither `-n` nor `-N` was passed (`line:col:text`, matching rg); previously a piped `st --column 'pat'` printed `col:text`. An explicit `-N --column` still prints `col:text` (divergence #17 resolved).
- **rg/grep fallback on a missing index is now the default.** Previously `st` exited 2 with guidance unless `--fallback`/`SYNTEXT_FALLBACK_RG=1` was set; now the search transparently runs `rg` (or `grep`). Disable with `SYNTEXT_FALLBACK_RG=0`; `--fallback` overrides the env var. **Scripts that parsed the exit-2 no-index error will see rg output and rg exit codes instead.** The notice stays suppressible via `-q`/`SYNTEXT_QUIET_FALLBACK`. Corrupt-index/lock failures still error loudly.

### Known divergences (see `tests/oracle/DIVERGENCES.md`)
- Binary repo files are never indexed (rg can report matches in them; `st` cannot: misses only, never false positives); `\r`-byte-matching patterns report `rg --crlf`-style spans.

### Changed
- Differential-oracle ripgrep pin bumped 15.1.0 → 15.2.0 (`tests/oracle/ORACLE_VERSION`, `EXPECTED_RG_VERSION`, CI rg install URLs in `ci.yml`/`nightly.yml`). All correctness/oracle suites re-baselined green against 15.2.0 with no behavioral divergence.

## [2.0.0] - 2026-07-11

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
- Fixed a trailing `\r` slicing panic in `verify_regex` by clamping `submatch_end` to `line_content_end`.
- Optimized same-line match checks in both `verify_literal` and `verify_regex` to run in $O(1)$ complexity by caching the previous line's end index and skipping backtracking to avoid adversarial $O(\text{line}^2)$ processing loops.
- Built a custom `verify_empty` verifier to directly match line boundaries for empty string searches (`st ""`), bypassing costly `memmem` scans and regex compilation.
- Standardized on standard-library file locking APIs (available since Rust 1.89) in `helpers.rs`, `update.rs`, `build.rs`, `delta_apply.rs`, and `compact.rs` and removed the `fs2` dependency completely.
- Updated the threat model comments in `open.rs` to correctly describe page behavior under `DictVerify::Structural`.
- Moved `gram_hashes()` in `mod.rs` to use `reader::read_exact_at` (pread path) on disk-backed native segments, protecting against `SIGBUS` or concurrency mutations during long compaction cycles.
- Aligned the double quotes backslash escaping logic in `shell.rs` with the POSIX standard, preserving backslashes inside double quotes verbatim unless escaping `$`, `` ` ``, `"`, or `\`.
- Added a 500ms read timeout via a worker thread to `read_stdin_json` in `protocols/mod.rs` to prevent hanging the editor tool calls when the stdin pipe remains open but stalled.
- Refactored `globs_in_argv_order` in `globs.rs` to check against lists of value-taking short and long flags so arguments (such as `-tg rs`) are not mistakenly parsed as glob flags.
- Throttled async catch-up spawning with a coarse TTL stamp so a burst of concurrent stale searches collapses to roughly one `st update` per window instead of stampeding the writer lock.
- Truncated UTF-16 files (odd byte count after the BOM) now decode the incomplete trailing code unit as U+FFFD instead of dropping it, matching ripgrep and removing an `-x` false-positive divergence (oracle fixture `repro_e1c1603c26349124`).
- `-x` (line-regexp) now matches CRLF mode like `rg --crlf`: a trailing `\r` at end-of-line is treated as part of the terminator, so `^pat$` matches a final line `pat\r` and submatch extraction stays consistent with the match decision (oracle fixture `repro_e1477df13c5a98f4`).
- JSON submatch enumeration strips a bare trailing `\r` from the final line before matching, so a CRLF-aware regex no longer emits a spurious empty submatch after the `\r` on empty-alternation queries like `parse|` under `-x` (oracle fixtures `repro_45977b47dc1f41aa`, `repro_bf18cfbef891f8f8`). The `\r` is still kept in the rendered line text.
- Resolved a predictable temporary file name TOCTOU vulnerability in `write_atomic` by using random UUIDs.
- Canonicalized directory paths before performing sensitive prefix checks in `validate_index_dir`.
- Structured verifier to count backward line-starts relative to a watermark to remove the quadratic $O(\text{matches} \times \text{file\_size})$ cost.
- Divert `--files-without-match` before the empty-results short-circuit and respect `-q` flag.
- Deduplicated `requeue_uncommitted` paths in `PendingEdits` to bound memory growth.
- Rejected literal and escaped newlines (`\n`, `\x0a`, etc.) in query patterns during routing.
- Warn on post-delta update errors instead of failing since the HEAD move is already durable.
- Suppressed confusing "no changes detected" output when a delta segment update successfully runs.
- Agent hook rewriter (`rg`/`grep` -> `st`) now re-emits a `--` separator before positionals, so a bare pattern equal to a subcommand name (`rg status .`) searches instead of silently routing to that subcommand, and a `--`-escaped leading-dash pattern (`rg -- -foo src`) no longer parses as a flag bundle. `grep --binary-files without-match` is now accepted space-separated (not just inline `=without-match`); other `--binary-files` values still abort the rewrite (semantics `st` does not replicate).

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
