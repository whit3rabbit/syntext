# Feature Specification: Hybrid Code Search Index

**Feature Branch**: `001-hybrid-code-search-index`
**Created**: 2026-03-25
**Revised**: 2026-03-27 (requirements review pass — all CHK001-CHK050 addressed)
**Status**: Draft
**Input**: PLAN.md research document on building a hybrid code search index in Rust for agent workflows

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Literal and Regex Search Across a Repository (Priority: P1)

An AI agent (or developer) runs a literal string or regex search against a local repository. The system uses an n-gram index to select candidate files, then verifies matches with the full pattern. Results return in under 50ms for warm queries, even on large repos where scan-based grep takes seconds.

**Why this priority**: This is the core value proposition. Without fast text search, the tool has no reason to exist. Agent workflows call grep repeatedly and in parallel; shaving seconds per call compounds into minutes saved per task.

**Independent Test**: Index a 1M+ LOC repository, run literal and regex queries, measure latency and correctness against ripgrep baseline.

**Acceptance Scenarios**:

1. **Given** an indexed repository, **When** a literal search is issued, **Then** results match ripgrep output (file paths, 1-based line numbers, and full line content) for all UTF-8 text files not excluded by .gitignore, and return in under 50ms warm cache (p95) on the reference hardware (Apple M1 or equivalent x86-64 with NVMe, 16GB RAM, repository resident in page cache).
2. **Given** an indexed repository, **When** a regex search is issued, **Then** the n-gram index narrows candidates before verification, and results are correct (no false negatives, no false positives in output).
3. **Given** a query with no extractable n-grams (e.g., `.*`), **Then** the system falls back to full scan gracefully and returns correct results (identical to ripgrep).
4. **Given** a case-sensitive query for a mixed-case token (e.g., `MyStruct`), **Then** results are correct: the lowercase index may produce a superset of candidates, but the verifier eliminates non-matching lines. Zero false negatives in final output.

---

### User Story 2 - Incremental Index Updates After File Edits (Priority: P1)

An agent edits files during a coding session. The index reflects those changes immediately so subsequent searches find newly written code ("read-your-writes" freshness).

**Why this priority**: Stale indexes cause agents to waste tokens searching for code they just wrote. Freshness is a hard requirement for agent workflows, not a nice-to-have.

**Independent Test**: Edit a file, call `commit_batch()`, query for the new content, verify the result appears without a full rebuild.

**Acceptance Scenarios**:

1. **Given** an indexed repo and a file edit, **When** `commit_batch()` returns, **Then** a `search()` call issued after `commit_batch()` MUST return the edited file for content present in the new version. A `search()` call issued before `commit_batch()` MUST NOT see uncommitted edits.
2. **Given** multiple rapid edits to different files, **When** searches interleave with edits, **Then** results are always consistent: a `search()` either sees all edits from a given `commit_batch()` call or none of them. No partial visibility of any batch.
3. **Given** a `notify_change()` followed immediately by a `search()` (no `commit_batch()`), **Then** the search MUST NOT see the pending change.

---

### User Story 3 - Path and File-Type Scoping (Priority: P2)

A user restricts search to specific paths or file types (e.g., only `.rs` files, only `src/`). The path index eliminates non-matching files before content search begins.

**Why this priority**: Path scoping is the cheapest filter and dramatically reduces candidate sets. Agents frequently scope by file type.

**Independent Test**: Run a scoped search and verify only files matching the path/type constraint appear in results.

**Acceptance Scenarios**:

1. **Given** a path scope `src/**/*.rs`, **When** a search is issued, **Then** only Rust files under `src/` are considered and only those files appear in results.
2. **Given** a file-type filter `-tpy`, **When** a search is issued, **Then** only Python files are searched.
3. **Given** a file-type exclusion `-T js`, **When** a search is issued, **Then** no `.js` files appear in results.

---

### User Story 4 - Symbol-Aware Search (Priority: P3)

A developer searches for a function definition or symbol reference. The system uses a symbol/AST index (tree-sitter or ctags-based) to return precise structural matches rather than raw text hits.

**Why this priority**: Symbol search is a precision layer on top of text search. Valuable for navigation but not required for the core grep-replacement use case.

**Independent Test**: Index a Rust file with known function definitions, query for a symbol, verify only definition sites (not string mentions) are returned.

**Acceptance Scenarios**:

1. **Given** a Rust file with `fn parse_query(...)`, **When** a symbol search for `parse_query` is issued (e.g., `sym:parse_query`), **Then** the definition location (file path + line number) is returned and the result is a structural definition — not a string literal or comment containing "parse_query".
2. **Given** a symbol search for an undefined name, **Then** no results are returned.
3. **Given** a file in an unsupported language (not in Tier 1: Rust, Python, TypeScript/JavaScript, Go, Java, C/C++), **When** a symbol search is issued, **Then** the heuristic fallback (regex-based definition detection) is used and results are labeled as "approximate".

---

### User Story 5 - Full Index Build from Scratch (Priority: P1)

A user initializes the index for a repository for the first time. The system builds path, content n-gram, and (optionally) symbol indexes. Build completes in under 5 seconds for typical repositories.

**Why this priority**: First-run experience. If initial indexing is too slow, users abandon the tool.

**Independent Test**: Time a full index build on repositories of varying sizes, verify all files are indexed correctly.

**Acceptance Scenarios**:

1. **Given** a repository with 100k LOC (typical developer project), **When** a full index build is triggered, **Then** it completes in under 5 seconds on a single developer machine (reference: Apple M1 or equivalent x86-64, NVMe, 16GB RAM). Note: SC-002 sets the formal bound at 500k LOC, 5 seconds; this scenario covers the more common case.
2. **Given** a repository with binary files and symlinks, **When** indexing, **Then** binary files (detected by null-byte scan of first 8KB) are skipped, and symlinks are followed exactly one level (no loop detection needed beyond standard OS limits, no out-of-repo traversal).
3. **Given** a repository with 0 files (empty repo or all files excluded by .gitignore), **When** building, **Then** an empty but valid index is created with `doc_count = 0` and no error is returned.
4. **Given** a repository where the index directory does not exist or is not writable, **When** `Index::open()` is called, **Then** `IndexError::Io` is returned with a message indicating the path and permission issue.

---

### Edge Cases

- **Zero-gram query**: A regex pattern that produces zero n-grams (e.g., `.*`, `.+`, `[a-z]`) falls back to full scan. Results are correct. No index structures are loaded.
- **Corrupt index**: A segment file with a bad magic byte, wrong version, or xxhash64 checksum mismatch triggers `IndexError::CorruptIndex`. The caller is responsible for rebuilding. The remaining non-corrupt segments continue to be usable if possible; a corrupt segment is skipped.
- **File deleted between index and query**: The overlay `delete_set` marks the base doc_id as deleted. The verifier skips attempts to open files that no longer exist (returns no results for that doc_id rather than an error).
- **File deleted then re-created**: Treated as a modify: `notify_change()` on the new path adds the new content to the overlay; `notify_delete()` on the original path (if called) removes old content. If only `notify_change()` is called (file re-created without explicit delete), the overlay doc supersedes the base doc for that path. No stale results.
- **Very large files (>10MB)**: Configurable `max_file_size` (default 10MB, specified in Config). Files exceeding this limit are skipped with a warning logged at `WARN` level. The default is a hard requirement, not just a note.
- **Non-UTF-8 files**: Files that fail UTF-8 validation are treated as binary and skipped. The binary detection heuristic (null-byte scan) runs first; if a file passes the binary check but fails UTF-8 decode, it is also skipped. "Limited search support" is not offered — non-UTF-8 files are skipped entirely. This is the measurable behavior.
- **`\r\n` line endings**: Files with Windows-style line endings are indexed normally. The verifier normalizes `\r\n` to `\n` when extracting line content for `SearchMatch.line_content`. Line numbers are counted by `\n` occurrences.
- **Very long lines (>10K chars)**: Indexed normally. The verifier reads the full line into memory. No truncation. If a line exceeds an internal buffer limit (none imposed in v1), the behavior is full read — memory is bounded by `max_file_size`.
- **Query during full reindex**: `Index::build()` writes a new manifest atomically at completion. In-flight `search()` calls continue against the pre-reindex snapshot until the ArcSwap is updated. No partial results. Queries issued after `build()` returns use the new index.
- **Overlay exceeds reindex threshold, reindex impossible**: If the overlay covers >30% of base files but `build()` cannot be initiated (e.g., disk full), searches continue against the current snapshot. No data loss. The caller receives a warning via `IndexStats.overlay_pct_of_base` exceeding the threshold; it is the caller's responsibility to decide when to rebuild.

---

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST build an n-gram content index from repository files.
- **FR-002**: System MUST build a path/filename index for scope filtering.
- **FR-003**: System MUST decompose regex patterns into n-gram queries (AND/OR) for candidate selection.
- **FR-004**: System MUST verify all candidates against the original pattern (no false positives in results).
- **FR-005**: System MUST support incremental updates via immutable segments + overlay.
- **FR-006**: System MUST use a linear-time regex engine by default to prevent ReDoS. The `regex` crate is the default. PCRE2 is explicitly deferred and, if compiled in as a future option, MUST be guarded by a feature flag; FR-006's linear-time guarantee applies only to the default path.
- **FR-007**: System MUST support literal string search as a fast path.
- **FR-008**: System SHOULD build a symbol/AST index for supported languages (tree-sitter, Tier 1: Rust, Python, TypeScript/JavaScript, Go, Java, C/C++). This feature is the same scope as US4 (P3). FR-008 and US4 describe the same capability; US4 acceptance scenarios are the acceptance criteria for FR-008.
- **FR-009**: System MUST store indexes locally on the user's machine (no server round-trips).
- **FR-010**: System MUST handle concurrent reads without blocking (immutable segments, lock-free reads via ArcSwap). Concurrent writes are serialized: `notify_change()` / `notify_delete()` acquire a `Mutex` on the pending buffer; `commit_batch()` is not concurrent-safe with itself (callers must serialize calls). This is compatible with US2 scenario 2: batch atomicity is provided by `commit_batch()`, not by simultaneous write parallelism.
- **FR-011**: System MUST skip binary files and respect .gitignore by default. Binary detection: scan the first 8KB of file content; if a null byte (`\x00`) is found, treat as binary and skip.
- **FR-012**: System SHOULD support sparse n-gram tokenization for improved selectivity. Sparse n-grams are the primary tokenization method (v1 default). FR-012 and FR-013 are independent: FR-012 is the core tokenization approach; FR-013 (phrase-aware masks) is an optional additive layer on top of FR-012's output.
- **FR-013**: System SHOULD support phrase-aware trigram masks (position + next-char bloom) for adjacency filtering. This is an optimization over FR-012, not a replacement. Acceptance scenario: given a query `foobar`, phrase-aware masks reduce false positives by eliminating documents that contain `foo` and `bar` as grams but not adjacently. This feature is optional (SHOULD); its absence does not violate correctness.
- **FR-014**: System MUST trigger a full reindex when the overlay covers more than 30% of base file count. This threshold MUST be configurable via `Config` (default: 0.30). The trigger is a recommendation surfaced via `IndexStats`; automatic reindex is the caller's responsibility.
- **FR-015**: System MUST handle the case where tree-sitter is unavailable or a grammar is missing at symbol index time. Behavior: fall back to the heuristic Tier 3 extractor (regex-based). If the heuristic extractor also fails, log a warning and skip that file. No error is returned to the caller for individual file failures during build.
- **FR-016**: System MUST detect corrupt or truncated segment files via magic byte and xxhash64 checksum validation on open. A corrupt segment MUST return `IndexError::CorruptIndex`. The system MUST remain operable on remaining valid segments (degraded mode, not full failure).
- **FR-017**: System MUST treat file re-creation (delete followed by create at the same path) as a modification. When `notify_change()` is called for a path already in the base index, the overlay supersedes the base entry via `delete_set` addition. The old base content is no longer returned in search results after the next `commit_batch()`.

### Error Response Requirements

The following error responses are required for all failure modes:

| Failure Mode | Error Returned | Behavior |
|---|---|---|
| Corrupt segment (bad magic, wrong version, checksum mismatch) | `IndexError::CorruptIndex(msg)` | Degrade: skip corrupt segment, continue with valid segments. Log at ERROR level. |
| Disk full during build or commit | `IndexError::Io(e)` where `e.kind() == ErrorKind::StorageFull` | Abort write. Leave previous valid manifest in place. Return error to caller. |
| Index directory missing or not writable | `IndexError::Io(e)` | Return immediately. Do not create partial state. |
| Pattern contains invalid regex syntax | `IndexError::InvalidPattern(msg)` | Return immediately. No index access. |
| Path outside repository root | `IndexError::PathOutsideRepo(path)` | Return immediately. Do not index the file. |
| File exceeds max_file_size | `IndexError::FileTooLarge { path, size }` | Skip file during build. Log at WARN level. Not returned from `search()`. |
| tree-sitter grammar missing | None (degraded) | Fall back to heuristic extractor. Log at WARN. |
| Unsupported regex pattern type (falls back to full scan) | None (not an error) | Route to full scan silently. Optionally log at DEBUG. |

### Key Entities

- **Segment**: Immutable index unit containing a gram dictionary and posting lists for a set of files.
- **Posting List**: Sorted list of document IDs (file IDs) associated with an n-gram key.
- **Overlay**: Small mutable segment representing uncommitted file changes on top of base segments.
- **Manifest**: Metadata file tracking active segments, their commit basis, and merge state.
- **Query Plan**: Internal representation of a search decomposed into gram lookups, set operations, and verification steps.

---

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Warm query latency under 50ms (p95) on repositories up to 1M LOC.
  - **Measurement methodology**: Benchmark on Apple M1 (or equivalent x86-64 with NVMe SSD, 16GB RAM). "Warm" = repository files have been accessed at least once in the prior 60 seconds so the OS page cache is populated. Measurement tool: Criterion (`cargo bench --bench query_latency`). Pattern mix: 50% literals, 40% indexed-regex, 10% full-scan. p50 target: <15ms. p99 target: <150ms. Repos above 1M LOC: no formal latency bound in v1; behavior should degrade gracefully (full scan still faster than ripgrep for scoped queries).

- **SC-002**: Full index build under 5 seconds for repositories up to 500k LOC.
  - **Measurement methodology**: Same reference hardware as SC-001. Parallelism: rayon default thread pool (all available cores). Measured from `Index::build()` call to return, including file enumeration, tokenization, segment write, and manifest update. Not including OS page cache warm-up. Single measurement (not averaged), worst-case file mix (all Rust source, no binary files to skip).

- **SC-003**: Incremental update latency under 100ms after a single file edit.
  - **Measurement boundary**: From the file's new content being present on disk (write complete) to `commit_batch()` returning. Does not include the time for the caller to detect the change (e.g., via `inotify`). A subsequent `search()` call after `commit_batch()` MUST reflect the new content (this is the read-your-writes guarantee, not measured separately as latency).

- **SC-004**: Search results are identical to ripgrep for all supported pattern types (correctness).
  - **Ripgrep version**: `ripgrep 14.x` (specifically the version pinned in `tests/integration/correctness.rs` via a `rg --version` assertion). The test suite MUST fail if the installed `rg` version differs from the pinned version.
  - **Scope**: SC-004 applies to UTF-8 text files within the repository that are not excluded by .gitignore and do not exceed `max_file_size`. Binary files, symlink targets, and .gitignored files are explicitly excluded from the SC-004 correctness guarantee.
  - **Case sensitivity**: SC-004 applies to case-sensitive queries. For case-insensitive queries (`-i` flag), results MUST be a superset of ripgrep's case-insensitive output (no false negatives). False positives for case-insensitive queries are acceptable if and only if they originate from the lowercase-normalization design decision documented in CLAUDE.md. The verifier eliminates them.
  - **Match definition**: "Identical" means: same set of file paths, same line numbers (1-based), same line content (full line, not just the matched portion). Byte offsets are not compared in the correctness harness (ripgrep's `--byte-offset` format is not tested).

- **SC-005**: Resident memory under 100MB for the dictionary of a 1M LOC repository.
  - **Scope**: The 100MB bound covers the mmap'd dictionary (gram hash table) across all active base segments. It does not include: posting list data (loaded on demand, not kept in memory), the overlay (bounded by dirty file count × average file size), path index Roaring bitmaps (typically <5MB), or the OS page cache. Total process RSS may exceed 100MB; the bound applies to the explicitly managed dictionary allocation.

- **SC-006**: No unsafe Rust without documented justification.
  - Any `unsafe` block MUST have a comment explaining: (1) why safe alternatives were insufficient, (2) the invariant that makes the operation sound, (3) the reviewer who approved it.

- **SC-007**: CLI exit codes are correct in all error conditions.
  - `0`: Matches found (search), or command succeeded (index/update/status).
  - `1`: No matches found (search only).
  - `2`: Error (invalid pattern, corrupt index, I/O error, permission denied). Any `IndexError` variant maps to exit code 2.

---

## Behavioral Requirements

### Concurrent Access

- `Index` is `Send + Sync`. Multiple threads MAY call `search()` concurrently.
- `notify_change()` and `notify_delete()` MAY be called from multiple threads; they serialize via an internal `Mutex<Vec<FileEdit>>`. These calls do NOT block `search()`.
- `commit_batch()` MUST NOT be called concurrently. Callers are responsible for serializing calls to `commit_batch()`. Concurrent calls produce undefined behavior (may panic or return an error in a future version).
- `build()` MUST NOT be called concurrently with `commit_batch()`. The manifest is atomic, but interleaved calls are not supported.

### Startup Validation

- On `Index::open()`, the system MUST validate that `config.index_dir` exists and is writable. If not, return `IndexError::Io` immediately.
- On `Index::open()`, the system MUST validate that `config.repo_root` exists. If not, return `IndexError::Io`.
- On `Index::open()`, if no manifest exists, create an empty index (valid state, zero documents). Do not return an error.

### Cardinality-Based Fallback

- When the smallest posting list for a query exceeds 10% of total indexed documents, the index is not used for that query. The query falls back to full scan.
- This threshold is configurable (future: `Config.scan_fallback_threshold`, default 0.10).
- The fallback is transparent to callers: results are correct regardless of which execution path is used.
- **Measurable criterion**: For a query with a known-high-frequency gram (e.g., `fn ` in a Rust repo), `Index::search()` MUST return correct results. Whether the index or full scan was used is observable via `RIPLINE_LOG_SELECTIVITY=1` environment variable output.

---

## Assumptions

- Target platform is macOS and Linux (desktop/server). Windows support is deferred.
- The tool runs locally, not as a server. No network protocol needed for v1.
- Repositories are primarily UTF-8 text. Binary file detection and skipping is sufficient.
- Git is available on the host for commit-based snapshot tracking (`ripline update` subcommand). If `git` is not available, `ripline update` returns an error; `ripline index` (full build) and `ripline search` function without git.
- tree-sitter grammars are available as optional dependencies for symbol indexing. If unavailable, the heuristic fallback is used (FR-015).
- tree-sitter grammar versions are treated as pinned Cargo dependencies (declared in `Cargo.toml` with exact versions under the `symbols` feature flag). Runtime-dynamic grammar loading is not supported in v1.
- The Rust `regex` crate is the default verification engine; PCRE2 support is optional/deferred and will not be added without an explicit feature flag.
- The index directory (`config.index_dir`, default `.ripline/`) MUST be on a local filesystem (not NFS, SMB, or other network filesystem). Network filesystems are not tested and their behavior (mmap support, atomic rename) is undefined for this tool.
- ripgrep `14.x` is the correctness baseline. Tests pin the version and fail if it differs.
- The index directory is assumed to be on the same filesystem as the repository root (for atomic rename semantics during manifest writes).

---

## Known Constraints & Design Resolutions

### Case-Sensitivity vs. Lowercase Normalization (CHK048)

The index stores grams from lowercased content. Case-sensitive queries (the default) produce correct results because:
1. The index returns a superset (all files containing the gram, regardless of original case).
2. The verifier re-checks the original pattern against original file content, eliminating false positives.

**Consequence**: Warm query latency for case-sensitive queries against mixed-case tokens is slightly higher than for all-lowercase tokens because the candidate set is larger. This is documented and accepted. SC-004 applies: zero false negatives, zero false positives in final output.

### 30% Overlay Threshold and 10% Cardinality Fallback (CHK050)

Both thresholds are promoted to functional requirements (FR-014 and the Cardinality-Based Fallback section above). Both are configurable. The 30% threshold governs when the caller should consider a full reindex. The 10% threshold governs query-time routing and is independent of overlay state.

### FR-006 Linear-Time Regex and Optional PCRE2 (CHK049)

FR-006's linear-time guarantee is unconditional for the default `regex` crate path. PCRE2 is deferred. If PCRE2 is added in a future version, it MUST be behind a `pcre2` feature flag and MUST NOT be the default. The linear-time guarantee in FR-006 applies only when the `pcre2` feature is not enabled.

### US5 Acceptance Scenario 1 vs. SC-002 (CHK018)

US5 scenario 1 ("100k LOC, under 5 seconds") and SC-002 ("500k LOC, under 5 seconds") are intentionally different targets. US5 scenario 1 describes the typical developer experience. SC-002 is the formal performance bound. A system that passes SC-002 automatically passes US5 scenario 1. There is no conflict.

### SC-003 Scope (CHK019)

SC-003's 100ms bound applies to a single file edit. Multi-file batch edits (N files) scale approximately linearly with N for small N and with dirty file content volume. No formal bound is specified for multi-file batches in v1. The `commit_batch()` rebuild cost for 100 dirty files averaging 20KB is approximately 5-20ms (see data-model.md OverlayView rebuild cost note).
