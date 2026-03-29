# Security Audit Log

Findings from security audits, with identification, remediation status, and
rationale for accepted risks.

## Threat Model

syntext is a local code search tool. The index directory is mode 0700 (owner
only). The primary threat is a compromised or malicious file within the
indexed repository, not a remote attacker. Attacks requiring write access to
the index directory are low-severity because that access already implies
owner-level compromise.

## Findings

### SA-001: TOCTOU Window in Symlink Walk

**Severity:** Medium
**Status:** Mitigated (defense-in-depth)
**File:** `src/index/walk.rs`, `collect_symlink_entry`

**Identification:** The symlink validation in `collect_symlink_entry` spans
multiple syscalls: `read_link`, `symlink_metadata`, `canonicalize`, and a
second `symlink_metadata`. Between the final validation and the build-time
file read for an in-repo file symlink, a concurrent symlink swap could
redirect to an out-of-scope target. Directory symlinks are now skipped during
repository enumeration, which removes the nested-walk portion of this risk.

**Remediation:**
1. Directory symlinks are skipped during repository enumeration, so the walker
   no longer performs nested sub-walks through symlink aliases.
2. The build pipeline (`src/index/build.rs`) already applies per-file
   `open_readonly_nofollow` + `verify_fd_matches_stat` inode verification,
   catching any remaining swaps between walk discovery and content read.
3. File symlinks still require the target to resolve inside the repo root, and
   multi-hop symlink chains are rejected before indexing.

**Residual risk:** The remaining window is between validation and the later
file open for an accepted in-repo file symlink. The build-time inode check is
the backstop.

### SA-002: V2 Posting Offset Lower Bound Too Permissive

**Severity:** Medium
**Status:** Fixed
**File:** `src/index/segment/mod.rs`, `read_posting_list_mmap`

**Identification:** For V2 combined segments, `read_posting_list_mmap`
validated that `abs_off >= HEADER_SIZE`, but the postings section starts after
the document table, not at `HEADER_SIZE`. A crafted V2 segment with a valid
checksum could embed a dictionary entry whose posting offset pointed into the
doc table region, causing doc table bytes to be interpreted as posting data.

**Remediation:**
1. The segment footer's `postings_offset` field (bytes 8..16) is now parsed
   and stored in `SegmentLayout` and `MmapSegment`.
2. `read_posting_list_mmap` uses `postings_offset` as the lower bound when
   non-zero (V2 segments that recorded it). Falls back to
   `doc_table_offset + doc_count * 8` (end of the doc table index array) as a
   conservative minimum for segments where `postings_offset` is zero.
3. `parse_segment_mmap` validates that `postings_offset` (when non-zero) falls
   within `[doc_table_offset, dict_offset]`.

### SA-003: Integer Underflow in Overlay Doc Count

**Severity:** Medium (cosmetic)
**Status:** Fixed
**File:** `src/index/overlay.rs`, `build_incremental`

**Identification:** The expression
`old_overlay.docs.len() + new_files.len() - newly_changed.len()` can underflow
when a path appears in both `newly_changed` and `removed_paths` (e.g.,
`notify_change` then `notify_delete` in the same batch). In release mode, the
bare subtraction wraps to `usize::MAX`, producing a misleading `DocIdOverflow`
error message. No memory corruption or privilege escalation results.

**Remediation:** Replaced bare arithmetic with `saturating_add` / `saturating_sub`
at both call sites in `build_incremental` (full rebuild and delta paths).

## Previously Addressed (for reference)

These were fixed in earlier commits and verified during this audit:

- **Path traversal** (c492ea4): `repo_relative_path` rejects `..`, absolute,
  and prefix components. `commit_batch` canonicalizes and checks
  `starts_with(canonical_root)`. Manifest filename validation rejects `/`, `\`,
  `..`, and absolute paths. Symbol search filters absolute and `..` paths.

- **ReDoS** (ec27f9d): 10 MiB NFA/DFA size cap on all regex compilation paths.
  The `regex` crate's RE2 engine guarantees linear-time matching.

- **Doc entry bounds** (c492ea4): `get_doc` validates `abs_off` within
  `[doc_table_offset, dict_offset)` and checks full variable-length entry
  (22-byte header + `path_len`) fits within the doc table region.

- **TOCTOU file reads** (c492ea4): `open_readonly_nofollow` + inode
  verification in `build.rs`, `commit_batch`, and the resolver hot path.

- **Symlink dedup** (0f3b6d9): `seen_canonical` set prevents duplicate file
  records when N symlinks point to the same in-repo file target.

## Verified Clean Areas

- **Unsafe blocks (2):** Both use `map_copy_read_only` (MAP_PRIVATE). Justified.
- **SQL injection:** Symbol index uses parameterized queries throughout.
- **Varint decoding:** Overflow guards on 5th byte and delta accumulation.
- **Concurrency:** ArcSwap snapshot isolation is correct. Poisoned mutex
  recovery is acceptable for idempotent bitmap caches.

## Round 2 Findings (2026-03-28)

### SA-004: Permissive Index Directory Mode Accepted Silently

**Severity:** High
**Status:** Fixed
**File:** `src/index/mod.rs`, `Index::open()`

**Identification:** The permissive-mode check only warned on stderr (gated
behind `config.verbose`). A pre-existing index with mode 0755 continued
operating with no user-visible signal.

**Remediation:** `Index::open()` now returns `CorruptIndex` when the index
directory has group/other bits set, unless `Config::strict_permissions` is
false. `build_index()` continues to enforce 0700 on new builds.

### SA-005: Lock Gap Between build_index and open

**Severity:** Medium
**Status:** Fixed
**File:** `src/index/build.rs`

**Identification:** The exclusive directory lock was dropped before `open()`
acquired a shared lock. Two concurrent builds could both succeed in the gap.

**Remediation:** The exclusive lock is downgraded to shared (unlock +
re-lock shared) while the writer lock is still held. The writer lock is
dropped only after the shared directory lock is acquired, closing the window.

### SA-006: segment_id Not Validated as UUID

**Severity:** Medium (latent)
**Status:** Fixed
**File:** `src/index/manifest.rs`, `Manifest::load()`

**Identification:** `segment_id` was not validated. While not currently used
in filesystem paths, a future code path could expose a path traversal.

**Remediation:** `Manifest::load()` validates that each `segment_id` parses
as a UUID.

### SA-007: MAX_POSTING_BYTES Allows 64 MB Per-Posting Allocation

**Severity:** Low
**Status:** Fixed
**File:** `src/index/segment/reader.rs`

**Identification:** A crafted `.post` file could force 64 MB allocation per
posting list. Multiple crafted grams in one query could exhaust memory.

**Remediation:** Reduced `MAX_POSTING_BYTES` from 64 MB to 8 MB. 8 MB
covers ~2M delta-varint-encoded doc_ids, well above any realistic segment.

### SA-008: Duplicate base64 Implementations

**Severity:** Low
**Status:** Fixed
**Files:** `src/cli/render.rs`, `tests/integration/cli.rs`

**Identification:** Two independent base64 implementations increased the
surface for encoding bugs in JSON output.

**Remediation:** Consolidated into `src/base64.rs` with RFC 4648 test vectors.

## Round 2 Accepted Risks

### AR-001: resolve_git_binary TOCTOU

**Severity:** Medium
**File:** `src/cli/manage.rs`
**Rationale:** Inherent to the Unix exec model. The canonical path refers to
the correct inode; only a binary replacement in a writable directory could
exploit this. `execveat(O_PATH)` would close the gap on Linux 3.19+ but is
not portable to macOS.

### AR-002: No Rate Limit on commit_batch Disk Writes

**Severity:** Low
**File:** `src/index/mod.rs`
**Rationale:** Requires API changes to Index (RateLimiter field or generation
cap). The overlay-full check (`OVERLAY_ENFORCE_THRESHOLD`) already bounds
total data growth. Rate limiting is a v2 consideration.

### AR-003: Thread-Local Buffer Sizing Under Large max_file_size

**Severity:** Low
**File:** `src/tokenizer/mod.rs`
**Rationale:** The shrink logic at `MIN_CAPACITY.max(needed * 4)` is correct.
Worst case is bounded by `max_file_size` (clamped to 1 GiB in SA-003 round 1).
Each rayon worker retains at most one buffer; rayon's default thread count is
bounded by CPU cores.
