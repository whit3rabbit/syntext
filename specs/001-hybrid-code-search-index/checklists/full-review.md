# Checklist: Full Requirements Review — Hybrid Code Search Index

**Purpose**: PR reviewer gate — validate completeness, clarity, consistency, and measurability of requirements before implementation review
**Created**: 2026-03-27
**Completed**: 2026-03-27 (all items resolved via spec.md requirements review pass)
**Feature**: specs/001-hybrid-code-search-index
**Audience**: PR reviewer
**Depth**: Full (functional + NFR, including P3/SHOULD requirements)

---

## Requirement Completeness

- [x] CHK001 — Are error response requirements defined for all failure modes (corrupt segment, disk full, missing index, unsupported pattern type)? [Completeness, Gap]
  _Resolved_: Added "Error Response Requirements" table in spec.md covering all failure modes with error type, behavior, and log level.

- [x] CHK002 — Is the full-reindex trigger threshold (30% overlay size) documented as a functional requirement rather than only a design note? [Completeness, Gap]
  _Resolved_: Promoted to FR-014 with configurable default (0.30) via Config.

- [x] CHK003 — Are requirements defined for concurrent write behavior, not just concurrent reads (FR-010)? [Completeness, Spec §FR-010]
  _Resolved_: FR-010 updated to document write serialization: notify_change/notify_delete use Mutex (non-blocking to readers); commit_batch must be serialized by caller; concurrent commit_batch calls are unsupported.

- [x] CHK004 — Is there a functional requirement covering corrupt or truncated segment recovery? (Edge cases section mentions it, but no FR captures it.) [Completeness, Gap]
  _Resolved_: Added FR-016: magic byte + xxhash64 checksum validation on open; corrupt segment returns CorruptIndex; remaining valid segments stay operable (degraded mode).

- [x] CHK005 — Are requirements defined for file deletion and re-creation — is it treated as an update or a delete-then-add? [Completeness, Gap]
  _Resolved_: Added FR-017 and "File deleted then re-created" edge case in spec.md. Treated as a modification: notify_change supersedes base entry via delete_set.

- [x] CHK006 — Are acceptance scenarios defined for FR-013 (phrase-aware trigram masks, SHOULD)? The requirement exists but has no corresponding user story or scenario. [Completeness, Spec §FR-013]
  _Resolved_: FR-013 updated with acceptance scenario: given query `foobar`, masks reduce false positives by eliminating docs containing `foo` and `bar` non-adjacently. Clarified as additive optimization over FR-012, not a replacement.

- [x] CHK007 — Are CLI exit code requirements specified for all error conditions? [Completeness, Gap]
  _Resolved_: Added SC-007 with complete exit code table: 0 (match/success), 1 (no match), 2 (any IndexError).

- [x] CHK008 — Are requirements defined for the behavior when tree-sitter is unavailable or a grammar is missing at symbol index time? [Completeness, Gap]
  _Resolved_: Added FR-015: fall back to Tier 3 heuristic extractor; if heuristic also fails, log WARN and skip file. No error returned to caller.

---

## Requirement Clarity

- [x] CHK009 — Is the measurement methodology for SC-001 (50ms p95 warm query) defined: hardware baseline, cache-warm definition, measurement tool? [Clarity, Spec §SC-001]
  _Resolved_: SC-001 now specifies: Apple M1 / equivalent x86-64 NVMe 16GB RAM; warm = files accessed in prior 60s; tool = Criterion; pattern mix 50/40/10%; p50 <15ms, p99 <150ms.

- [x] CHK010 — Is the measurement methodology for SC-002 (5s full build, 500k LOC) defined: single-core or parallel, hardware spec? [Clarity, Spec §SC-002]
  _Resolved_: SC-002 now specifies: same hardware as SC-001; rayon default thread pool (all cores); measured from Index::build() call to return; single worst-case measurement.

- [x] CHK011 — Is "identical to ripgrep" (SC-004) scoped to a specific ripgrep version and flag set? Without pinning, the baseline can shift. [Clarity, Spec §SC-004]
  _Resolved_: SC-004 specifies ripgrep 14.x; test suite pins version via `rg --version` assertion in correctness.rs and fails on mismatch.

- [x] CHK012 — Does SC-004 apply to case-sensitive queries only, or also case-insensitive? Given lowercase normalization at index time, case-insensitive parity requires clarification. [Clarity, Spec §SC-004]
  _Resolved_: SC-004 explicitly applies to case-sensitive queries (zero false negatives). For case-insensitive queries, results must be a superset (no false negatives); false positives from lowercase normalization are eliminated by the verifier.

- [x] CHK013 — Is "read-your-writes freshness" (US2) expressed as a measurable requirement (e.g., search issued after `commit_batch()` returns MUST see the new content)? [Clarity, Spec §US2]
  _Resolved_: US2 scenarios rewritten with precise commit_batch() boundary semantics and explicit "MUST" language.

- [x] CHK014 — Is SC-003 (100ms incremental update) measured from file write to search returning the new result, or only to overlay commit? The boundary matters for implementation scope. [Clarity, Spec §SC-003]
  _Resolved_: SC-003 boundary defined as: from file write complete on disk to commit_batch() returning. Does not include change detection latency. Post-commit search correctness is a separate guarantee (not a latency bound).

- [x] CHK015 — Is the definition of "binary file" specified (e.g., heuristic byte threshold, null-byte presence)? FR-011 says skip binaries but does not define the detection rule. [Clarity, Spec §FR-011]
  _Resolved_: FR-011 updated: binary detection = scan first 8KB; if null byte (\x00) found, treat as binary and skip.

- [x] CHK016 — Is "handled safely" for symlinks (US5 scenario 2) defined with measurable criteria (no loop, no out-of-repo traversal, warning vs. skip)? [Clarity, Spec §US5]
  _Resolved_: US5 scenario 2 updated: symlinks are followed exactly one level; no loop detection beyond OS limits needed; out-of-repo traversal is prevented by the ignore crate's behavior.

- [x] CHK017 — Is the relationship between FR-012 (sparse n-gram tokenization) and FR-013 (phrase-aware trigram masks) specified? Are these independent features or layers of the same mechanism? [Clarity, Spec §FR-012, §FR-013]
  _Resolved_: FR-012 and FR-013 are explicitly independent. FR-012 is the core tokenization method; FR-013 is an optional additive optimization layer. Presence of FR-013 does not affect FR-012 behavior.

---

## Requirement Consistency

- [x] CHK018 — US5 acceptance scenario 1 states "under 5 seconds" for 100k LOC; SC-002 states "under 5 seconds" for 500k LOC. Are these consistent targets or intentionally different? [Consistency, Spec §US5, §SC-002]
  _Resolved_: "Known Constraints & Design Resolutions" section documents the intentional difference: US5 scenario 1 = typical developer experience; SC-002 = formal bound. Passing SC-002 implies passing US5 scenario 1. No conflict.

- [x] CHK019 — FR-005 specifies incremental updates via segments + overlay; SC-003 covers a single file edit. Is the latency bound in SC-003 also intended to cover multi-file batch edits? [Consistency, Spec §FR-005, §SC-003]
  _Resolved_: SC-003 scope explicitly limited to single file edit. Multi-file batch cost documented as approximately linear; no formal bound in v1.

- [x] CHK020 — FR-010 guarantees concurrent reads without blocking; US2 scenario 2 requires no partial states during interleaved edits and searches. Are these two requirements using compatible concurrency models? [Consistency, Spec §FR-010, §US2]
  _Resolved_: FR-010 updated to document that batch atomicity (US2 scenario 2) is provided by commit_batch() snapshot swap, not by concurrent write parallelism. The models are compatible: reads are lock-free; writes are serialized but non-blocking to readers.

- [x] CHK021 — FR-008 (symbol index, SHOULD) and US4 (P3) both describe symbol search but use different acceptance criteria. Do they describe the same scope? [Consistency, Spec §FR-008, §US4]
  _Resolved_: FR-008 explicitly states it covers the same scope as US4, and that US4 acceptance scenarios are the acceptance criteria for FR-008.

---

## Acceptance Criteria Quality

- [x] CHK022 — Does SC-004 ("identical to ripgrep") apply to binary files, symlinks, and .gitignored files, or only to UTF-8 text files within the repo? The boundary is unstated. [Acceptance Criteria, Spec §SC-004]
  _Resolved_: SC-004 explicitly scoped to UTF-8 text files not excluded by .gitignore and not exceeding max_file_size. Binary files, symlink targets, and .gitignored files explicitly excluded from SC-004 guarantee.

- [x] CHK023 — Does SC-005 (100MB memory limit) cover only the gram dictionary, or also posting lists, the overlay, and the path index? The scope of the bound is unstated. [Acceptance Criteria, Spec §SC-005]
  _Resolved_: SC-005 scope defined: covers mmap'd dictionary (gram hash table) across all active base segments. Explicitly excludes: posting list data (loaded on demand), overlay, path index Roaring bitmaps.

- [x] CHK024 — Is "results match ripgrep output" (US1 scenario 1) defined in terms of file paths only, or also line numbers and matched text? [Acceptance Criteria, Spec §US1]
  _Resolved_: SC-004 "Match definition" clause specifies: same file paths + same line numbers (1-based) + same line content (full line). Byte offsets excluded from correctness harness comparison.

- [x] CHK025 — Is the edge case "non-UTF-8 files: skip or index as binary with limited search support" expressed as a verifiable acceptance criterion? "Limited search support" is not measurable. [Acceptance Criteria, Gap]
  _Resolved_: Edge case updated: non-UTF-8 files are skipped entirely (no "limited support"). Binary check (null-byte) runs first; UTF-8 decode failure also causes skip. Measurable: these files never appear in search results.

- [x] CHK026 — Are the acceptance criteria for US4 (symbol search) sufficient to distinguish a symbol definition hit from a string literal hit, which is the core value of the feature? [Acceptance Criteria, Spec §US4]
  _Resolved_: US4 scenario 1 updated to explicitly require that the result is a structural definition, not a string literal or comment containing the symbol name.

- [x] CHK027 — Is there a measurable acceptance criterion for the cardinality-based fallback (skip index when smallest posting list > 10% of total docs)? [Acceptance Criteria, Gap]
  _Resolved_: "Cardinality-Based Fallback" section added with measurable criterion: for a known high-frequency gram query, results must be correct regardless of path taken. Observable via SYNTEXT_LOG_SELECTIVITY=1.

---

## Scenario Coverage

- [x] CHK028 — Are requirements defined for the cold-cache query path? SC-001 specifies warm cache only; cold-start latency is not bounded. [Coverage, Spec §SC-001]
  _Resolved_: SC-001 explicitly scoped to warm cache. Cold-cache latency is not bounded in v1 (behavior: degrades gracefully; full scan still faster than ripgrep for scoped queries above 1M LOC).

- [x] CHK029 — Is the file-deleted-between-index-and-query scenario captured in an FR or SC, not only in the edge cases prose? [Coverage, Gap]
  _Resolved_: Edge case "File deleted between index and query" updated with measurable behavior: delete_set marks base doc_id deleted; verifier skips missing files (no error, no result for that doc_id).

- [x] CHK030 — Is a scenario defined for a query issued while a full reindex is in progress? [Coverage, Gap]
  _Resolved_: "Query during full reindex" edge case added: build() writes new manifest atomically at completion; in-flight search() calls continue against pre-reindex snapshot; no partial results.

- [x] CHK031 — Are recovery scenarios defined for crashes mid-segment-write? The architecture mentions on-disk generations for crash recovery, but no requirement specifies the expected post-crash state. [Coverage, Gap]
  _Resolved_: build() and commit_batch() use write-then-rename for manifest; a crash mid-write leaves the previous manifest valid. On-disk overlay generations enable recovery of committed-but-not-consolidated changes. FR-016 covers corrupt segment detection on next open.

- [x] CHK032 — Are requirements defined for the overlay when it exceeds the reindex threshold but a full reindex cannot be initiated (e.g., repo is locked or disk is full)? [Coverage, Edge Case]
  _Resolved_: "Overlay exceeds reindex threshold, reindex impossible" edge case added: searches continue against current snapshot; caller is informed via IndexStats.overlay_pct_of_base; no data loss; no automatic action taken.

---

## Edge Case Coverage

- [x] CHK033 — Is the default file size limit (skip files >10MB) specified as a requirement with a default value, or is it only referenced as "configurable"? [Edge Case, Gap]
  _Resolved_: FR-011 and Config.max_file_size specify default 10MB as a hard requirement. Exceeding files skipped with WARN log. Error Response table includes FileTooLarge entry.

- [x] CHK034 — Are requirements defined for files with \r\n line endings? They appear in the test fixture corpus (T009) but not in any FR or acceptance scenario. [Edge Case, Gap]
  _Resolved_: Edge case "\r\n line endings" added: indexed normally; verifier normalizes \r\n to \n in line_content; line numbers counted by \n occurrences.

- [x] CHK035 — Are very long line requirements defined (>10K chars per line)? Test fixtures include them but behavior is unspecified in requirements. [Edge Case, Gap]
  _Resolved_: Edge case "Very long lines" added: indexed normally; verifier reads full line; no truncation; no imposed buffer limit in v1.

- [x] CHK036 — Is behavior defined for an empty repository (zero files)? [Edge Case, Gap]
  _Resolved_: US5 scenario 3 added: empty repo produces valid index with doc_count=0, no error.

- [x] CHK037 — Are requirements defined for a repository root that is a network filesystem (NFS, SMB)? The assumptions section covers macOS/Linux but not filesystem type. [Edge Case, Spec §Assumptions]
  _Resolved_: Assumptions updated: index directory must be on local filesystem (NFS/SMB not supported; mmap and atomic rename behavior undefined on network filesystems).

---

## Non-Functional Requirements

- [x] CHK038 — Are p50 and p99 query latency bounds defined in addition to the p95 bound in SC-001? [NFR, Spec §SC-001]
  _Resolved_: SC-001 measurement methodology adds p50 target <15ms and p99 target <150ms alongside the p95 <50ms bound.

- [x] CHK039 — Is a query latency bound defined for repositories above 1M LOC, not just at the 1M LOC level? [NFR, Spec §SC-001]
  _Resolved_: SC-001 documents explicitly: repos above 1M LOC have no formal latency bound in v1; behavior degrades gracefully.

- [x] CHK040 — Is a peak memory bound defined for the index build process? The architecture notes ~1.5GB per 256MB batch but no SC captures this. [NFR, Gap]
  _Resolved_: SC-002 measurement methodology notes that peak memory during build is ~1.5GB per 256MB batch (documented in ARCHITECTURE.md). This is an architectural characteristic, not a formally bounded SC in v1. The 256MB batch size is configurable to tune memory usage.

- [x] CHK041 — Is an on-disk storage size requirement defined for the index relative to the source corpus? [NFR, Gap]
  _Resolved_: FR-026/post-build assertion (T026) warns if index >0.5x corpus size. This is the operative bound: not a hard error, but a warning that the weight table may be poor. Documented in tasks.md and ARCHITECTURE.md.

- [x] CHK042 — Is there a requirement defining behavior when PCRE2 is optionally enabled alongside the linear-time default (FR-006)? [NFR, Spec §FR-006]
  _Resolved_: FR-006 updated: PCRE2 is deferred; if added, must be behind a pcre2 feature flag; linear-time guarantee applies only when pcre2 feature is disabled. Documented in "Known Constraints" section.

- [x] CHK043 — Is there a graceful degradation requirement for low-memory conditions? [NFR, Gap]
  _Resolved_: Low-memory conditions surface as IndexError::Io (e.g., mmap fails, allocation fails). No explicit OOM handling beyond what the OS and Rust allocator provide. Graceful degradation = return error, not crash. Documented in Error Response table under Io variant.

---

## Dependencies & Assumptions

- [x] CHK044 — Is the assumption that ripgrep is available for correctness testing pinned to a minimum version? [Assumption, Spec §Assumptions]
  _Resolved_: Assumptions and SC-004 specify ripgrep 14.x. Test suite asserts version at runtime.

- [x] CHK045 — Is the assumption "Git is available on the host" covered by a fallback requirement if git is absent? [Assumption, Spec §Assumptions]
  _Resolved_: Assumptions updated: `st update` requires git; if absent, returns error. `st index` and `st search` function without git.

- [x] CHK046 — Are the tree-sitter grammar versions treated as dependencies with pinned versions in Cargo.toml, or only as optional runtime availability? [Dependency, Gap]
  _Resolved_: Assumptions updated: grammar versions are pinned Cargo dependencies under the `symbols` feature flag. Runtime-dynamic loading not supported in v1.

- [x] CHK047 — Is the assumption that the index directory is writable and local explicitly validated at startup, with a defined error path? [Assumption, Gap]
  _Resolved_: "Startup Validation" section in Behavioral Requirements: Index::open() MUST validate index_dir exists and is writable; if not, return IndexError::Io immediately.

---

## Ambiguities & Conflicts

- [x] CHK048 — "Identical to ripgrep" (SC-004) combined with "lowercase normalization at index time" (design) implies case-sensitive queries will produce false negatives for mixed-case tokens. Is this acknowledged and bounded in the spec? [Ambiguity, Conflict, Spec §SC-004]
  _Resolved_: "Known Constraints — Case-Sensitivity vs. Lowercase Normalization" section added. Zero false negatives is guaranteed because the verifier re-checks original content. The index returns a superset; normalization does not cause missed results.

- [x] CHK049 — FR-006 mandates linear-time regex by default; the assumptions section defers PCRE2 as optional. If PCRE2 is compiled in, does FR-006 still apply? This is unresolved. [Ambiguity, Spec §FR-006]
  _Resolved_: FR-006 and "Known Constraints — FR-006 Linear-Time Regex and Optional PCRE2" section: linear-time guarantee applies only when pcre2 feature is disabled. FR-006 does not apply to the PCRE2 path.

- [x] CHK050 — The 30% full-reindex threshold and the cardinality-based query fallback (>10% of total docs) are both defined only in design notes, not in requirements. Should both be promoted to FRs with configurable defaults? [Ambiguity, Gap]
  _Resolved_: 30% threshold promoted to FR-014 (configurable). 10% cardinality fallback documented in "Cardinality-Based Fallback" behavioral requirement section (configurable, future Config field noted).
