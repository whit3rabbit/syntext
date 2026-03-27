# Tasks: Hybrid Code Search Index

**Input**: Design documents from `/specs/001-hybrid-code-search-index/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/

**Tests**: Included. SC-004 (ripgrep correctness) is a hard acceptance criterion. Test harness is built in Phase 2 per implementation order constraints.

**Organization**: Tasks grouped by user story. US5 (Build) before US1 (Search) because an index must exist before it can be queried.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

---

## Phase 1: Setup

**Purpose**: Cargo project initialization, dependency configuration, directory structure

- [x] T001 Initialize Cargo project with `cargo init --lib`, add `[[bin]]` target for CLI in Cargo.toml
- [x] T002 Add all dependencies to Cargo.toml: regex, regex-syntax, memchr, memmap2, roaring, rayon, zerocopy, arc-swap, ignore, clap (derive), uuid, serde/serde_json, xxhash-rust; dev-deps: criterion, tempfile
- [x] T003 [P] Create module directory structure per plan.md: src/tokenizer/, src/index/, src/posting/, src/query/, src/search/, src/path/, src/symbol/, src/cli/
- [x] T004 [P] Create test directory structure: tests/integration/, tests/unit/, benches/
- [x] T005 [P] Create stub mod.rs files with module declarations in src/lib.rs and each subdirectory
- [x] T006 [P] Define core public types (Config, SearchMatch, SearchOptions, IndexStats, IndexError) in src/lib.rs per contracts/library-api.md
- [x] T007 Verify `cargo check` passes with all stubs and dependencies

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Weight table, correctness harness, tokenizer, and posting lists. Every user story depends on these.

**NON-NEGOTIABLE ORDER**: T008 (weight table) must complete before T012 (tokenizer). T009-T011 (test harness) must complete before any index implementation.

### Weight Table (MUST be first)

- [x] T008 Generate pre-trained byte-pair frequency weight table in src/tokenizer/weights.rs: download mixed-language source (Rust, Python, TypeScript, Go, Java), count all 65536 byte-pair frequencies on lowercased content, invert to weights (rare = high), emit `pub const WEIGHTS: [u16; 65536]` constant. Validate by eyeballing grams produced for sample files.

### Ripgrep Correctness Harness (MUST be before index)

- [x] T009 Create fixture repository in tests/fixtures/corpus/ with 50-100 files: unicode identifiers, empty files, whitespace-only files, very long lines (>10K chars), binary-looking text that is actually valid UTF-8, files with \r\n line endings, nested directories, .gitignore with ignored files, symlinks, files >10MB (should be skipped)
- [x] T010 [P] Build ripgrep oracle test harness in tests/integration/correctness.rs: run `rg` on fixture repo for a set of test patterns (literals, regexes, case-insensitive, no-match), capture output, store as expected results
- [x] T011 [P] Define test pattern set in tests/integration/correctness.rs covering: exact literals, regex with alternation, regex with repetition, case-insensitive literal, pattern matching no files, pattern with unicode, `(foo)?bar` optional prefix pattern, `.*` fallback-to-scan pattern

### Tokenizer

- [x] T012 Implement sparse n-gram tokenizer trait and `build_all()` in src/tokenizer/mod.rs: lowercase normalization, boundary detection using WEIGHTS table, recursive extraction down to trigrams. Per research.md section 1.
- [x] T013 Implement `build_covering()` query-time gram extraction in src/tokenizer/mod.rs: greedy left-to-right longest qualifying gram, O(query_length). Per research.md section 1.
- [x] T014 Implement gram hashing function in src/tokenizer/mod.rs: hash variable-length gram bytes to u64 for dictionary lookup
- [x] T015 Write unit tests for tokenizer in tests/unit/tokenizer.rs: verify boundary detection, lowercase normalization, covering set minimality, round-trip (grams from build_all cover build_covering output), edge cases (empty string, single byte, all-same-char)

### Posting Lists

- [x] T016 [P] Implement delta-varint encoding/decoding for posting lists in src/posting/mod.rs: encode sorted u32 doc_ids as delta + varint, streaming decode iterator. Per research.md section 3.
- [x] T017 [P] Implement Roaring bitmap wrapper in src/posting/roaring_util.rs: threshold-based switching (>8K entries), serialization/deserialization compatible with roaring crate format
- [x] T018 Implement adaptive intersection in src/posting/mod.rs: linear merge for similar sizes, galloping for ratio >32:1. Early termination on empty. Per research.md section 3.
- [x] T019 [P] Implement k-way union via min-heap in src/posting/mod.rs for OR queries
- [x] T020 Write unit tests for posting lists in tests/unit/posting.rs: encode/decode round-trip, intersection correctness (equal size, skewed size, empty, disjoint), union correctness, Roaring threshold switching

**Checkpoint**: Foundation ready. Weight table validated, correctness harness captures ripgrep expected output, tokenizer produces good grams, posting list ops are correct.

---

## Phase 3: User Story 5 - Full Index Build from Scratch (Priority: P1) MVP-Build

**Goal**: Build path + content n-gram indexes for a repository. Completes in under 5s for typical repos. Skips binaries, respects .gitignore.

**Independent Test**: Run `ripline index --stats` on fixture repo, verify all non-ignored text files are indexed and stats are printed.

### Implementation for US5

- [x] T021 [US5] Implement RPLX segment writer in src/index/segment.rs: write header, document table, postings section (delta-varint or Roaring per list), page-aligned dictionary section, TOC footer with xxhash64 checksum. Per contracts/segment-format.md.
- [x] T022 [US5] Implement RPLX segment reader in src/index/segment.rs: mmap file, verify magic/version/checksum, binary search dictionary, read posting list at offset. Return CorruptIndex on validation failure.
- [x] T023 [US5] Implement manifest read/write in src/index/manifest.rs: JSON serialization of Manifest struct, atomic write-then-rename, GC for orphan segment files. Per data-model.md Manifest entity.
- [x] T024 [US5] Implement PathIndex builder in src/path/mod.rs: enumerate files via ignore crate walker, build sorted path list, populate extension_to_files and component_to_files Roaring bitmaps. Per data-model.md PathIndex entity.
- [x] T025 [US5] Implement batched-segment build pipeline in src/index/mod.rs: split files into ~256MB batches, per batch: rayon parallel file read + lowercase + sparse gram extraction, sort (gram_hash, doc_id) pairs, sequential emit posting lists, write segment. Per research.md section 17.
- [x] T026 [US5] Implement `Index::build()` in src/index/mod.rs: file enumeration via ignore crate (respects .gitignore, skips binaries, enforces max_file_size), batched-segment build, path index build, manifest write, post-build size assertion (warn if >0.5x corpus). Per library-api.md.
- [x] T027 [US5] Implement `Index::open()` in src/index/mod.rs: load manifest, mmap base segments, rebuild path index, create initial empty IndexSnapshot via ArcSwap. Per library-api.md.
- [x] T028 [US5] Write integration test in tests/integration/index_build.rs: build index on fixture repo, verify doc_count matches expected, verify binary files skipped, verify .gitignored files skipped, verify segment file exists with valid RPLX header.

**Checkpoint**: `Index::open()` and `Index::build()` work. Segments are written to disk and can be mmap'd and read back. Path index is populated.

---

## Phase 4: User Story 1 - Literal and Regex Search (Priority: P1) MVP-Search

**Goal**: Search indexed repos with literal or regex patterns. Results match ripgrep. Sub-50ms warm queries.

**Independent Test**: Run correctness harness (T010-T011) against `Index::search()`, verify results identical to ripgrep for all test patterns.

### Implementation for US1

- [x] T029 [US1] Implement GramQuery enum and simplification rules in src/query/mod.rs: And, Or, Grams, All, None variants. Simplification: And removes All, Or with All becomes All, empty And/Or, single-child unwrap. Per data-model.md GramQuery entity.
- [x] T030 [US1] Implement HIR walker for regex decomposition in src/query/regex_decompose.rs: parse with regex_syntax, walk HirKind (Literal -> Grams, Concat -> And, Alternation -> Or, Repetition min>=1 -> recurse, min=0 -> All, Class/Look/Empty -> All). Grams uses build_covering() from tokenizer. Per research.md section 4.
- [x] T031 [US1] Implement query router in src/query/mod.rs: detect literal (no regex metacharacters) vs indexed regex (HIR yields grams) vs full scan (All). Return QueryRoute enum. Per data-model.md QueryRoute entity.
- [x] T032 [US1] Implement GramQuery direct execution against segments in src/search/mod.rs: And nodes intersect, Or nodes union, Grams nodes load posting lists sorted by ascending cardinality with early termination. Query base segments + overlay. Per data-model.md QueryExecution.
- [x] T033 [US1] Implement tiered verifier in src/search/verifier.rs: literal path uses memchr::memmem, regex path uses compiled regex crate Regex. Returns Vec<SearchMatch> with correct line numbers, line content, byte offsets. Per contracts/library-api.md SearchMatch.
- [x] T034 [US1] Implement `Index::search()` in src/search/mod.rs: acquire ArcSwap snapshot, route query, execute gram query on base + overlay, subtract delete_set, union candidates, verify, sort by path then line number. Per library-api.md.
- [x] T035 [US1] Write unit tests for regex decomposition in tests/unit/query.rs: literal "foo" -> Grams, `foo.*bar` -> And(Grams(foo), Grams(bar)), `(foo|bar)` -> Or(Grams(foo), Grams(bar)), `(foo)?bar` -> Grams(bar) only (CRITICAL: verify optional prefix does not contribute grams), `.*` -> All, `foo+` -> Grams(foo)
- [x] T036 [US1] Run ripgrep correctness harness (T010-T011) against Index::search() in tests/integration/correctness.rs: for each test pattern, compare ripline output to stored ripgrep expected output. Fail on any difference in paths, line numbers, or line content.

**Checkpoint**: `Index::search()` returns correct results for literals and regex. Correctness validated against ripgrep oracle.

---

## Phase 5: User Story 2 - Incremental Index Updates (Priority: P1)

**Goal**: After file edits, searches immediately reflect changes without full rebuild. Batch commits are atomic.

**Independent Test**: Build index, modify a file, call notify_change + commit_batch, search for new content, verify it appears.

### Implementation for US2

- [x] T037 [US2] Implement OverlayView builder in src/index/overlay.rs: given a list of dirty files (path + content), compute sparse grams for each, build HashMap<u64, Vec<u32>> gram index, assign overlay doc_ids (disjoint from base range). Per data-model.md OverlayView.
- [x] T038 [US2] Implement IndexSnapshot and ArcSwap integration in src/index/overlay.rs: IndexSnapshot struct holding base_segments, merged_overlay, delete_set, path_index. ArcSwap<Arc<IndexSnapshot>> for atomic swap. search() clones Arc at start. Per research.md section 7.
- [x] T039 [US2] Implement `notify_change()` and `notify_delete()` in src/index/overlay.rs: buffer FileEdit in Mutex<Vec<FileEdit>>. Check .gitignore via ignore crate and silently skip ignored files. Per library-api.md guarantees 6.
- [x] T040 [US2] Implement `commit_batch()` in src/index/overlay.rs: take pending edits, rebuild full OverlayView from ALL dirty files (not just new batch), compute delete_set (base doc_ids for modified/deleted files), create new IndexSnapshot, ArcSwap::store(). Write on-disk generation file for crash recovery. Per research.md section 7.
- [x] T041 [US2] Implement `notify_change_immediate()` convenience method in src/index/overlay.rs: notify_change + commit_batch.
- [ ] T042 [US2] Implement on-startup overlay recovery in src/index/mod.rs: if manifest references overlay file, load it. If missing/corrupt, detect dirty files via `git diff` against base_commit. Per data-model.md Manifest. (DEFERRED: crash recovery is not needed for the core overlay API; implemented as needed in Phase 9.)
- [x] T043 [US2] Write unit tests for overlay in tests/unit/overlay.rs: single file add, single file modify (verify old grams removed, new grams present), file delete, batch atomicity (pending edits invisible until commit), ArcSwap snapshot isolation (in-flight search sees old snapshot).
- [x] T044 [US2] Write integration test in tests/integration/incremental.rs: build index, modify file, commit_batch, search for new content (must find), search for old content (must not find in modified file), verify interleaved edit+search consistency.

**Checkpoint**: Overlay system provides read-your-writes freshness with atomic batch commits and snapshot isolation.

---

## Phase 6: User Story 3 - Path and File-Type Scoping (Priority: P2)

**Goal**: Restrict search to specific paths or file types. Path filter executes first as Roaring bitmap AND.

**Independent Test**: Search with `-t rs` flag, verify only .rs files in results. Search with path glob `src/`, verify only files under src/.

### Implementation for US3

- [x] T045 [US3] Implement path/type glob filter in src/path/filter.rs: parse glob pattern into component matches + extension match, intersect Roaring bitmaps from PathIndex to produce candidate file_id set. Support `-t`/`-T` type filters and path glob patterns.
- [x] T046 [US3] Integrate path filter into search executor in src/search/mod.rs: when SearchOptions has path_filter or file_type, compute file_id bitmap from PathIndex, AND with posting list intersection results before verification.
- [x] T047 [US3] Write integration test in tests/integration/index_build.rs (extend): search with path filter `src/**/*.rs` on fixture repo, verify only matching files appear. Search with `-t py`, verify only .py files. Search with `-T js`, verify .js files excluded.

**Checkpoint**: Path and type scoping works as first-stage filter, dramatically reducing candidate sets for scoped queries.

---

## Phase 7: User Story 4 - Symbol-Aware Search (Priority: P3)

**Goal**: Search for function definitions and symbol references using Tree-sitter parse trees and SQLite storage.

**Independent Test**: Index a Rust file with known functions, query `sym:parse_query`, verify definition location returned.

### Implementation for US4

- [ ] T048 [US4] Implement Tree-sitter symbol extractor in src/symbol/extractor.rs: parse file with language-specific grammar, walk tree to extract (name, kind, line, column, span) for functions, structs, classes, traits, methods, enums. Support Tier 1 languages: Rust, Python, TypeScript/JavaScript, Go, Java, C/C++. Wrap parse in panic catch.
- [ ] T049 [US4] Implement Tier 3 heuristic fallback in src/symbol/extractor.rs: regex-based definition extraction (`^\s*(def|fn|func|function|class|struct|enum|trait|interface)\s+(\w+)`) for unsupported languages.
- [ ] T050 [US4] Implement SQLite symbol index in src/symbol/mod.rs: create/open WAL-mode database, schema per research.md section 11 (symbols table with name, kind, file_id, line, column, language; indexes on name, kind+name, file_id). Bulk insert during build, incremental update on overlay commit.
- [ ] T051 [US4] Implement `search_symbols()` method in src/symbol/mod.rs: query SQLite by name (with LIKE for prefix match), optionally filter by kind and language. Return SearchMatch results.
- [ ] T052 [US4] Integrate symbol index build into `Index::build()` in src/index/mod.rs: after content index build, run symbol extraction for Tier 1 languages, bulk insert into SQLite.
- [ ] T053 [US4] Integrate symbol search into query router in src/query/mod.rs: detect `sym:`, `def:`, `ref:` prefixes, route to SymbolSearch path.
- [ ] T054 [US4] Write integration test for symbol search: index a Rust file with `fn parse_query(...)`, query `sym:parse_query`, verify definition line returned. Query `sym:nonexistent`, verify empty results.

**Checkpoint**: Symbol search works as a separate mode for Tier 1 languages with heuristic fallback for others.

---

## Phase 8: CLI

**Purpose**: Wire library API to command-line interface per contracts/cli.md.

- [x] T055 Implement `ripline index` subcommand in src/cli/mod.rs: parse --force, --stats, --quiet flags, call Index::open() + Index::build(). Print stats if requested. Per contracts/cli.md.
- [x] T056 [P] Implement `ripline search` subcommand in src/cli/mod.rs: parse pattern, path args, -l/-i/-t/-T/-m/-c/--json/-q flags, call Index::search(). Format output as grep-compatible `path:line:content` or JSON. Exit codes 0/1/2 per contracts/cli.md.
- [x] T057 [P] Implement `ripline status` subcommand in src/cli/mod.rs: call Index::stats(), format output (plain or --json). Per contracts/cli.md.
- [x] T058 [P] Implement `ripline update` subcommand in src/cli/mod.rs: detect changed files via git diff against base_commit, call notify_change for each + commit_batch. --flush flag forces compact(). Per contracts/cli.md.
- [x] T059 Implement global options in src/cli/mod.rs: --index-dir, --repo-root (auto-detect via .git), -v/--verbose, environment variable overrides (RIPLINE_INDEX_DIR, RIPLINE_MAX_FILE_SIZE). Per contracts/cli.md.
- [x] T060 Implement main.rs binary entry point: parse clap commands, dispatch to subcommand handlers, handle errors with appropriate exit codes.

**Checkpoint**: Full CLI works end-to-end. `ripline index && ripline search "pattern"` produces correct grep-compatible output.

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Benchmarks, edge cases, hardening

- [ ] T061 [P] Create query latency benchmark in benches/query_latency.rs: decomposed per-phase (dictionary lookup, posting intersection, verification) on a realistic corpus. Criterion groups for literal vs regex vs full scan. (DEFERRED: benchmarks require larger corpus)
- [ ] T062 [P] Create selectivity benchmark in benches/selectivity.rs: measure candidate set size as percentage of total files for various pattern types. Assert <0.5% for 95th percentile. (DEFERRED)
- [ ] T063 [P] Create index build benchmark in benches/index_build.rs: measure throughput (MB/s) and peak memory for repos of 100MB, 500MB, 1GB. (DEFERRED)
- [ ] T064 Implement background segment merge in src/index/merge.rs: merge smallest segments when count exceeds max_segments config. Rebuild posting lists excluding deleted doc_ids. Atomic manifest swap. Per research.md section 2. (DEFERRED: single segment adequate for v1)
- [x] T065 [P] Add doc comments to all public API types and methods in src/lib.rs per quality gate 4.
- [x] T066 Run `cargo clippy` and fix all warnings per quality gate 2.
- [x] T067 Verify no source file exceeds 400 lines (test files exempt) per constitution principle V.
- [ ] T068 Run full correctness suite (T036) on a large real-world repo (e.g., ripgrep source, 50K+ LOC) to validate at scale. (DEFERRED: requires cloning external repos)
- [x] T069 Add .ripline/ to default .gitignore template recommendations in README or docs.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: No dependencies, start immediately
- **Phase 2 (Foundational)**: Depends on Phase 1. T008 (weight table) MUST complete before T012 (tokenizer). T009-T011 (test harness) MUST complete before Phase 3.
- **Phase 3 (US5 Build)**: Depends on Phase 2 complete
- **Phase 4 (US1 Search)**: Depends on Phase 3 (needs built index to query)
- **Phase 5 (US2 Incremental)**: Depends on Phase 4 (needs search to verify overlay works)
- **Phase 6 (US3 Path Scoping)**: Depends on Phase 3 (path index built in US5) + Phase 4 (search integration)
- **Phase 7 (US4 Symbol)**: Depends on Phase 3 (build integration). Can parallelize with US2/US3.
- **Phase 8 (CLI)**: Depends on Phases 3-5 minimum. T056-T058 can parallelize.
- **Phase 9 (Polish)**: Depends on Phases 3-8 complete

### User Story Dependencies

- **US5 (Build)**: Foundation only. No story dependencies. First story to implement.
- **US1 (Search)**: Depends on US5 (needs index to exist).
- **US2 (Incremental)**: Depends on US1 (needs search to verify freshness).
- **US3 (Path Scoping)**: Depends on US5 (path index) + US1 (search integration). Independent of US2.
- **US4 (Symbol)**: Depends on US5 (build integration). Independent of US1-US3 for build phase; needs US1 for search integration.

### Within Each User Story

- Segment format before build pipeline
- Build pipeline before search
- Query decomposition before search executor
- Verifier before search executor
- Core implementation before integration tests

### Parallel Opportunities

- **Phase 1**: T003, T004, T005, T006 all parallel (different directories/files)
- **Phase 2**: T009, T010, T011 parallel with each other; T016, T017, T019 parallel (different posting list files)
- **Phase 4**: T029, T030, T031 can start in parallel (different query/ files), converge at T032
- **Phase 7**: Entire US4 can parallelize with US2 and US3 if US5 is complete
- **Phase 8**: T056, T057, T058 parallel (different subcommands, same file but independent functions)
- **Phase 9**: T061, T062, T063, T065 all parallel

---

## Parallel Example: Phase 2 Foundational

```
# Weight table (MUST be first, blocks tokenizer):
Agent 1: T008 - Generate weight table

# After T008 completes, tokenizer + test harness + posting lists in parallel:
Agent 1: T012, T013, T014, T015 (tokenizer, sequential)
Agent 2: T009, T010, T011 (test harness, sequential)
Agent 3: T016, T017 (posting list encoding, parallel)
Agent 4: T018, T019, T020 (posting list ops, sequential after T016/T017)
```

## Parallel Example: Phase 4 US1 Search

```
# Query decomposition components (parallel, different files):
Agent 1: T029 - GramQuery enum in src/query/mod.rs
Agent 2: T030 - HIR walker in src/query/regex_decompose.rs
Agent 3: T033 - Verifier in src/search/verifier.rs

# After above converge:
Agent 1: T031 - Query router (depends on T029, T030)
Agent 2: T032 - Search executor (depends on T029, T033)

# Integration:
Agent 1: T034 - Index::search() (depends on T031, T032)
Agent 1: T035, T036 - Tests
```

---

## Implementation Strategy

### MVP First (US5 + US1)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (weight table first!)
3. Complete Phase 3: US5 (Build) -- index exists on disk
4. Complete Phase 4: US1 (Search) -- queries work, validated against ripgrep
5. **STOP and VALIDATE**: Run correctness harness. If it passes, you have a working grep replacement.

### Incremental Delivery

1. Setup + Foundational -> foundation ready
2. US5 (Build) + US1 (Search) -> working indexed search (MVP)
3. US2 (Incremental) -> agent-friendly freshness
4. US3 (Path Scoping) -> scoped search
5. US4 (Symbol) -> definition/reference search
6. CLI + Polish -> release-ready

### Suggested MVP Scope

**US5 + US1 only.** This gives you a tool that can `ripline index && ripline search "pattern"` and return correct results faster than ripgrep on large repos. Everything else (overlay, path scoping, symbols) is incremental value on top of a working core.

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story
- Weight table (T008) is the absolute first implementation task. Everything depends on good grams.
- Ripgrep correctness harness (T009-T011) must pass before any index work. This is the oracle.
- `(foo)?bar` correctly produces `Grams("bar")` only in T030/T035. This is NOT a bug. Test it.
- Post-build assertion (T026) warns if index > 0.5x corpus size. Catches bad weight tables.
- All gram hashes use lowercased content. Case-sensitive queries still work (verifier filters).
- Commit after each task or logical group.
