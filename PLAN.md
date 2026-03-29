# syntext v1.0 Release Plan

**Date**: 2026-03-29
**Current version**: 0.1.0
**Current state**: Phases 1–6 and 8 complete. Phase 7 (Symbols) partially stubbed. Phase 9 (Polish) in progress.

This plan identifies everything that must be resolved — bugs, missing features, hardening, documentation, and testing — before the project can ship a 1.0 release that users and tool integrators can depend on.

---

## 1. Current State Summary

### What works today

- Full index build from repository files (sparse n-gram tokenizer, batched segments, SNTX v3 format)
- Literal and regex search with ripgrep-validated correctness (SC-004)
- Incremental overlay updates with batch commit and ArcSwap snapshot isolation
- Path/type scoping via Roaring bitmap component index
- CLI (`st`) with grep-compatible output, NDJSON, context lines, heading mode, invert match
- Encoding normalization (UTF-8 BOM stripping, UTF-16 LE/BE transcoding)
- Security hardening (O_NOFOLLOW, inode verification, path traversal rejection, MAP_PRIVATE mmap, advisory locking, directory permissions, symlink escape prevention)
- Compaction (selective segment rewrite from snapshot)
- Calibrated scan threshold (index-vs-scan crossover measured at build time)
- Symbol extraction stubbed behind `--features symbols` (tree-sitter + SQLite)

### What is incomplete or broken

- 25 bugs/issues identified across two review passes (categorized below)
- Symbol search (US4/Phase 7) is partially implemented but tasks T048–T054 are incomplete
- Crash recovery (T042) deferred
- Background segment merge (T064) deferred
- Large-corpus correctness validation (T068) never run
- Benchmark suite (T061–T063) deferred
- No `cargo publish` dry-run or crate metadata validation
- No CHANGELOG
- README benchmarks are snapshot numbers, not CI-reproducible

---

## 2. Bug Triage

Every bug from both review passes, categorized by release-blocking severity.

### P0 — Must fix before 1.0 (correctness or data corruption)

| # | Bug | Location | Impact |
|---|-----|----------|--------|
| B01 | `base_doc_id_limit` silently swallows checked_add overflow via filter_map, returning a too-low limit. Overlay doc_ids can collide with base doc_ids. | `src/index/mod.rs` | **Silent data corruption**: two different documents share a doc_id. Queries return wrong results. |
| B02 | `varint_encode` accepts duplicate doc_ids (`[0, 0]` passes `w[0] <= w[1]`) but `varint_decode` rejects them (zero delta error). Round-trip violation. | `src/posting/mod.rs` | Posting list written by encode cannot be read back by decode. Corrupt segment on disk. |
| B03 | `read_posting_list_mmap` lower bound checks `abs_off < HEADER_SIZE` instead of validating against the actual postings section start. SA-002 fix is incomplete. | `src/index/segment/mod.rs` | Crafted V2 segment with valid checksum can cause doc table bytes to be interpreted as posting data. |
| B04 | `open_inner` does not validate that `base_doc_id` ranges across segments are non-overlapping. Crafted manifest with two segments at `base_doc_id: 0` silently drops one segment's documents. | `src/index/mod.rs` | Silent data loss: overlapping segments make documents invisible to path lookups and delete_set. |
| B05 | `build_incremental` doc count arithmetic `old_overlay.docs.len() + new_files.len() - newly_changed.len()` can underflow. SA-003 claims saturating arithmetic but code still uses bare subtraction. | `src/index/overlay.rs` | Panic in debug, wrap to usize::MAX in release → misleading DocIdOverflow error. |
| B06 | `commit_batch` uses `self.config.max_file_size + 1` (bare addition) instead of `saturating_add(1)`. Library consumers can set `max_file_size = u64::MAX`. | `src/index/mod.rs` | Wraps to `take(0)`, reads zero bytes. File appears empty, grams are lost, silent false negatives. |
| B07 | `path_matches_glob` with `**/test` prefix falls through to `memmem::find(path, b"test")` — substring match instead of component-boundary match. | `src/path/filter.rs` | `**/test` matches `src/contest.rs`. Wrong files included in scoped searches. |
| B08 | `calibrate_threshold` divides by `sample_count` which is zero when `indexed_paths` is empty. | `src/index/build.rs` | Division by zero panic on empty repository build. |

### P1 — Should fix before 1.0 (performance, security hardening, robustness)

| # | Bug | Location | Impact |
|---|-----|----------|--------|
| B09 | `should_use_index` clones a potentially large RoaringBitmap (`posting_bitmap(...).as_ref().clone()`) on every indexed query's hot path. | `src/search/mod.rs` | Unnecessary allocation for common grams (tens of thousands of entries). Measurable latency cost. |
| B10 | `projected_overlay_doc_count` can miscount when `visible_changed` and `removed_paths` overlap. | `src/index/mod.rs` | Spurious `OverlayFull` rejection or `debug_assert_eq!` failure. |
| B11 | `normalize_encoding` silently drops trailing byte on odd-length UTF-16 files (truncated on disk). | `src/index/encoding.rs` | Indexed content differs from verified content by one character. Potential false negative. |
| B12 | `cmd_update` per-file `exists()` check races with `commit_batch` read. One unreadable file aborts entire batch. | `src/cli/manage.rs` | User edits 50 files, deletes 1 before commit — all 50 changes lost. |
| B13 | `compact_index` reads `base_ids` from snapshot but validates against manifest-derived bases. Concurrent `commit_batch` could cause divergence. | `src/index/compact.rs` | Compaction assigns wrong global doc_ids. Silent corruption of rewritten segments. |
| B14 | `collect_symlink_entry` spawns unbounded nested `WalkBuilder` instances for distinct symlink targets. | `src/index/walk.rs` | Pathological repo with many symlinks can exhaust file descriptors or memory. |
| B15 | `render_invert_match` only inverts within candidate files, not the full corpus. Semantic mismatch with `rg -v` / `grep -v`. | `src/cli/render.rs` | `st -v TODO` returns wrong results. Users expect corpus-wide inversion. |
| B16 | Search with `-m N` (max results) verifies all candidates in parallel, then truncates. No early exit. | `src/search/mod.rs` | `-m 1` on a common term verifies thousands of files unnecessarily. |
| B17 | `build_incremental_delta` clones entire `gram_index` HashMap even for single-file edits. Cost grows with overlay size. | `src/index/overlay.rs` | Commit latency regresses as overlay grows. Documented but not mitigated. |
| B18 | Thread-local buffer in tokenizer returns `buf.clone()` every call. Allocation saved but copy not. | `src/tokenizer/mod.rs` | Hot path during rayon parallel build allocates and copies per file. |

### P2 — Acceptable for 1.0 with documentation (known limitations)

| # | Bug | Location | Impact |
|---|-----|----------|--------|
| B19 | `PostingList::len()` is O(n) for Small variant. Doc comment warns but no compile-time guard. | `src/posting/mod.rs` | Future code calling `.len()` on hot path would regress. Low risk today. |
| B20 | `Manifest::gc_orphan_segments` can race with concurrent `open()` that loaded old manifest. | `src/index/manifest.rs` | Unix inode semantics keep mmap valid. No functional impact on supported platforms. |
| B21 | `for_each_line` doesn't handle bare `\r` (classic Mac line endings). | `src/search/lines.rs` | Matches ripgrep behavior. Vanishingly rare in modern code. |
| B22 | `SegmentWriter::serialize` mutates internal state via sort+dedup. | `src/index/segment/segment_writer.rs` | Surprising but not incorrect. Second call produces same output. |
| B23 | `compute_delete_set` redundantly iterates overlapping modified/deleted paths. | `src/index/pending.rs` | Roaring insert is idempotent. No functional impact. |
| B24 | Overlay content normalization contract (callers must pre-normalize) is undocumented. | `src/index/overlay.rs` | Only `commit_batch` constructs overlays with user content. Tests could violate. |
| B25 | `sym:` with empty name returns all symbols. | `src/symbol/mod.rs` | Potentially useful behavior. Document it or reject empty queries. |

---

## 3. Missing Features for 1.0

### 3a. Symbol search (US4 / Phase 7) — REQUIRED for 1.0

The spec lists US4 as P3, but the feature flag, SQLite schema, extractor, and query routing are already partially implemented. Shipping 1.0 with a `--features symbols` flag that doesn't fully work is worse than either completing it or removing the stubs. **Decision: complete it.**

| Task | Description | Status | Estimate |
|------|-------------|--------|----------|
| T048 | Tree-sitter symbol extractor for Tier 1 languages (Rust, Python, TS/JS, Go, Java, C/C++) | **Done** (in `src/symbol/extractor.rs`) | — |
| T049 | Tier 3 heuristic fallback (regex-based) | **Done** (in `src/symbol/extractor.rs`) | — |
| T050 | SQLite symbol index: schema, WAL mode, bulk insert, incremental update | **Done** (in `src/symbol/mod.rs`) | — |
| T051 | `search_symbols()` method | **Done** (in `src/symbol/mod.rs`) | — |
| T052 | Integrate symbol build into `Index::build()` | **Done** (in `src/index/build.rs`) | — |
| T053 | Route `sym:`, `def:`, `ref:` prefixes to symbol search | **Done** (in `src/query/mod.rs` + `src/index/mod.rs`) | — |
| T054 | Integration test for symbol search | **Done** (in `tests/integration/symbols.rs`) | — |

On closer inspection, all T048–T054 tasks appear to be implemented. The tasks.md status markers are stale. **Action: verify `cargo test --features symbols` passes, then mark T048–T054 as complete in tasks.md.**

### 3b. Crash recovery (T042) — DEFER to 1.1

On-startup overlay recovery from on-disk generation files. The current behavior (empty overlay on restart, stale index until `st update` or `st index --force`) is acceptable for a local developer tool. Document this limitation.

### 3c. Background segment merge (T064) — DEFER to 1.1

Single segment per batch is adequate for repos under 1M LOC. Compaction (`Index::compact()`) already handles the multi-segment case. Background merge is an optimization, not a correctness requirement.

### 3d. Large-corpus correctness validation (T068) — REQUIRED for 1.0

Must run the full correctness harness (`tests/integration/correctness.rs`) against at least one real-world repo with 50K+ files. The benchmark presets already exist. This is a validation gate, not a code change.

### 3e. Benchmark suite on larger corpus (T061–T063) — REQUIRED for 1.0

The README publishes benchmark numbers. Those numbers must be reproducible from the committed benchmark harness. The Criterion benches exist but only run on the synthetic 300-file corpus. The external harness (`scripts/bench_compare.py`) with preset catalog must be validated and documented as the canonical benchmark method.

---

## 4. Hardening and Robustness

### 4a. Error recovery in `cmd_update`

`cmd_update` should catch per-file errors during `commit_batch` rather than aborting the entire batch. This requires either:
- Splitting the commit into per-file commits (expensive, breaks atomicity), or
- Pre-validating files (exists, readable, within size limit) before calling `notify_change`, and skipping failures with a warning

The second approach is preferred.

### 4b. Symlink walk depth limit

Add a configurable limit (default: 1) on the depth of nested `WalkBuilder` instances spawned by `collect_symlink_entry`. This prevents pathological repos from exhausting file descriptors.

### 4c. Invert match correctness

`st -v` must either:
- Walk all indexed files (correct but slow), or
- Be documented as "invert within matching files only" and renamed/flagged differently, or
- Be removed from the CLI until implemented correctly

Recommendation: implement corpus-wide inversion using the PathIndex to enumerate all files. The path index already has every file; the cost is O(indexed_files) which is the same as a full scan.

### 4d. Early exit for `--max-count`

Add an `AtomicUsize` counter shared across rayon tasks. Each task checks the counter before verifying a candidate. When the counter reaches `max_results`, remaining tasks skip verification. This preserves parallelism for the common case while avoiding wasted work.

---

## 5. Documentation for 1.0

### 5a. Required documentation changes

| Document | Change |
|----------|--------|
| `README.md` | Update project status to 1.0. Remove "under active development" caveat. Verify all benchmark numbers are current. |
| `CHANGELOG.md` | **Create.** Document all changes from 0.1.0 to 1.0. |
| `docs/ARCHITECTURE.md` | Verify all quantitative claims match current implementation. Update any stale numbers. |
| `CLAUDE.md` | Update implementation order section (all phases complete). Remove "suggested next task" note. |
| `specs/001-hybrid-code-search-index/tasks.md` | Mark T048–T054 as complete. Update deferred task rationale. |
| `src/lib.rs` | Add crate-level documentation with usage examples. Ensure all public types have doc comments (T065 claims done — verify). |
| `Cargo.toml` | Add `readme`, `keywords`, `categories` fields for crates.io. Verify `description` is accurate. |

### 5b. Known limitations to document

These are acceptable behaviors that must be explicitly documented rather than discovered by users:

1. **Crash recovery**: Overlay state is lost on unclean shutdown. Run `st update` or `st index` after a crash.
2. **Invert match scope**: `st -v` inverts within candidate files only (if not fixed per 4c).
3. **Non-aligned substring coverage**: ~16% false-negative rate for queries that don't align with token boundaries. Token-aligned queries (identifiers, keywords) have 0% false negatives.
4. **Network filesystems**: Index directory must be on local filesystem. NFS/SMB behavior is undefined.
5. **Case-insensitive overhead**: ~15–20% more candidates due to lowercase normalization. Correct results guaranteed by verifier.
6. **`\r`-only line endings**: Treated as single line (matches ripgrep behavior).
7. **Symbol search accuracy**: Tier 3 (heuristic) results are approximate. Tree-sitter failures fall back silently.

---

## 6. Testing for 1.0

### 6a. Existing test coverage to verify

| Test suite | Command | Status |
|------------|---------|--------|
| Unit: tokenizer | `cargo test --test tokenizer` | Passes |
| Unit: posting | `cargo test --test posting` | **Must verify after B02 fix** |
| Unit: query | `cargo test --test query` | Passes |
| Unit: overlay | `cargo test --test overlay` | **Must verify after B05 fix** |
| Unit: boundary_fuzz | `cargo test --test boundary_fuzz` | Passes |
| Integration: index_build | `cargo test --test index_build` | Passes |
| Integration: incremental | `cargo test --test incremental` | Passes |
| Integration: correctness | `cargo test --test correctness` | Passes (requires `rg` on PATH) |
| Integration: cli | `cargo test --test cli` | Passes |
| Integration: symbols | `cargo test --features symbols --test symbols` | **Must verify** |
| Clippy | `cargo clippy -- -D warnings` | Passes |

### 6b. New tests required for 1.0

| Test | Covers bug | Description |
|------|-----------|-------------|
| `base_doc_id_overflow_returns_error` | B01 | `base_doc_id_limit` with near-u32::MAX values must return error, not silently drop |
| `varint_encode_rejects_duplicates` | B02 | Change encode to use strict `<` check; add test for `[0, 0]` rejection |
| `v2_posting_offset_validates_against_postings_start` | B03 | Craft segment with dict entry pointing into doc table; verify rejection |
| `overlapping_base_doc_ids_rejected_on_open` | B04 | Manifest with two segments at base_doc_id 0; verify error |
| `build_incremental_no_underflow` | B05 | Trigger the `newly_changed > old + new` case; verify no panic |
| `max_file_size_u64_max_commit_batch` | B06 | Library consumer sets `max_file_size = u64::MAX`; verify no wrap |
| `glob_double_star_bare_word_component_match` | B07 | `**/test` must not match `contest.rs` |
| `calibrate_threshold_empty_paths` | B08 | Empty repo produces default threshold, no panic |
| `real_repo_correctness` | T068 | Run correctness harness on React or Rust compiler repo |

### 6c. Fuzz testing gate

The existing `cargo-fuzz` target (`fuzz_coverage_invariant`) must run for at least 10 minutes with no failures before 1.0 release. Current coverage: 1.45M executions with 0 violations.

---

## 7. Release Checklist

### Pre-release validation

- [x] All P0 bugs (B01-B08) fixed with regression tests
- [x] All P1 bugs either fixed or documented with tracking issues
- [x] `cargo test` passes (all test suites)
- [x] `cargo test --features symbols` passes
- [x] `cargo clippy -- -D warnings` passes
- [ ] No source file exceeds 400 lines (test files exempt) — **known violation**: 6 files exceed limit (largest: `src/index/mod.rs` ~1865 lines). Constraint is aspirational; splits deferred to v1.1.
- [ ] Fuzz target runs 10 minutes with 0 failures
- [ ] Correctness harness passes on fixture corpus
- [ ] Correctness harness passes on at least one external repo (50K+ files)
- [ ] Benchmark presets produce consistent, reproducible numbers
- [ ] `cargo publish --dry-run` succeeds

### Documentation

- [x] CHANGELOG.md created
- [x] README.md updated (status, benchmarks current, version 1.0)
- [ ] All public APIs have doc comments
- [x] Known limitations documented in README and ARCHITECTURE.md
- [x] Cargo.toml metadata complete for crates.io

### Release

- [x] Version bumped to `1.0.0` in Cargo.toml
- [ ] Git tag `v1.0.0`
- [ ] CI release workflow (`release.yml`) tested with a pre-release tag
- [ ] GitHub Release with binaries (Linux amd64/arm64, macOS x86_64/arm64)
- [ ] `.deb` packages built and attached
- [ ] `cargo publish` to crates.io

---

## 8. Execution Plan

### Phase A: Bug fixes (P0) — ~3 days

All P0 bugs are localized to specific functions. Fix them sequentially with a regression test for each.

1. **B01** (`base_doc_id_limit`): Change `filter_map` to return `Result`, propagate `DocIdOverflow`.
2. **B02** (`varint_encode`): Change `<=` to `<` in the sorted check. Add explicit duplicate rejection. Fix any call sites that depend on the old behavior.
3. **B03** (`read_posting_list_mmap`): Compute actual postings section start from `doc_table_offset + doc_count * 8 + variable_doc_entries_size`, or store `postings_offset` in SegmentLayout and use it as the lower bound (SA-002's intended fix).
4. **B04** (`open_inner`): After loading all segments, verify that `[base_id, base_id + doc_count)` ranges do not overlap. Return `CorruptIndex` on overlap.
5. **B05** (`build_incremental`): Replace bare subtraction with `saturating_sub`. Add test that triggers the edge case.
6. **B06** (`commit_batch`): Replace `+ 1` with `.saturating_add(1)`.
7. **B07** (`path_matches_glob`): After stripping `**/`, check if the remainder has no `/` — if so, use `path_has_component` instead of `memmem::find`.
8. **B08** (`calibrate_threshold`): Early return `0.10` when `indexed_paths.is_empty()`.

### Phase B: P1 fixes and hardening — ~3 days

1. **B09**: Use `&=` with borrowed bitmap instead of clone. Or use Roaring's `intersection_len()` to check selectivity without materializing the intersection.
2. **B10**: Deduplicate `visible_changed` against `removed_paths` before projection.
3. **B11**: Check `chunks_exact(2).remainder()` length; if non-zero, log a warning.
4. **B12**: Pre-validate files in `cmd_update` before committing. Skip unreadable files with warning.
5. **B13**: Take exclusive lock in `compact_index` before reading snapshot, ensuring consistency.
6. **B14**: Add depth counter to `collect_symlink_entry`; cap at 1 level of symlink-to-directory nesting.
7. **B15**: Implement corpus-wide invert match using PathIndex file enumeration.
8. **B16**: Add `AtomicUsize` counter for early exit in parallel search with `--max-count`.
9. **B17**: Document the clone cost; add a comment noting Cow/persistent-map as v2 optimization.
10. **B18**: Document the clone; defer optimization to v2 (callback pattern would change the API).

### Phase C: Symbol search validation — ~1 day

1. Run `cargo test --features symbols --test symbols` and fix any failures.
2. Verify `st search "sym:parse_query"` works on the fixture corpus.
3. Update tasks.md to mark T048–T054 complete.
4. Write one additional integration test: index a multi-language corpus (fixture corpus), query `sym:` for symbols across Rust/Python/TypeScript/Java, verify results.

### Phase D: Large-corpus validation — ~1 day

1. Clone React repo (preset `react_token_aligned`).
2. Run `st index --stats` and verify build completes.
3. Run correctness harness patterns against the React index.
4. Run `scripts/bench_compare.py --preset react_token_aligned` and verify count matches.
5. Repeat with one additional preset (Rust compiler or TypeScript).
6. Record results in `docs/BENCHMARKS.md`.

### Phase E: Documentation and release prep — ~2 days

1. Create `CHANGELOG.md` with all notable changes since 0.1.0.
2. Update `README.md`: remove "under active development", update status table, verify benchmark numbers.
3. Update `Cargo.toml`: add `readme`, `keywords`, `categories`.
4. Verify all public API doc comments are present and accurate.
5. Add "Known Limitations" section to README.
6. Run `cargo publish --dry-run`.
7. Test release workflow with a `v1.0.0-rc1` tag.
8. Bump version to `1.0.0`, tag, release.

### Timeline

| Phase | Duration | Dependencies |
|-------|----------|-------------|
| A: P0 bug fixes | 3 days | None |
| B: P1 fixes + hardening | 3 days | After A |
| C: Symbol validation | 1 day | After A |
| D: Large-corpus validation | 1 day | After A, B |
| E: Documentation + release | 2 days | After A, B, C, D |
| **Total** | **~10 working days** | |

Phases B and C can run in parallel. Phase D requires A and B to be complete (bug fixes affect correctness results).

---

## 9. What is explicitly NOT in 1.0

These are tracked as v1.1+ work and must not block the release:

- **Crash recovery** (T042): overlay generation files on disk, startup recovery
- **Background segment merge** (T064): automatic compaction in a background thread
- **FM-index alternative**: valid v2 path, 10x slower construction
- **Content-defined chunking**: block-level positional data for sub-file granularity
- **Dual dictionary** (case-sensitive + case-insensitive): ~2x dictionary size
- **Overlapping trigrams**: ~3.5x index size increase for non-aligned substring coverage
- **PCRE2 support**: behind feature flag, deferred indefinitely
- **Windows support**: compile_error! in io_util.rs, deliberate
- **Rate limiting on commit_batch**: accepted risk AR-002
- **Persistent overlay (Cow/persistent map)**: optimization for large overlays
- **Two-file dictionary-only mmap**: separate dictionary from postings for large indexes