# Feature Specification: Hybrid Code Search Index

**Feature Branch**: `001-hybrid-code-search-index`
**Created**: 2026-03-25
**Status**: Draft
**Input**: PLAN.md research document on building a hybrid code search index in Rust for agent workflows

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Literal and Regex Search Across a Repository (Priority: P1)

An AI agent (or developer) runs a literal string or regex search against a local repository. The system uses an n-gram index to select candidate files, then verifies matches with the full pattern. Results return in under 50ms for warm queries, even on large repos where scan-based grep takes seconds.

**Why this priority**: This is the core value proposition. Without fast text search, the tool has no reason to exist. Agent workflows call grep repeatedly and in parallel; shaving seconds per call compounds into minutes saved per task.

**Independent Test**: Index a 1M+ LOC repository, run literal and regex queries, measure latency and correctness against ripgrep baseline.

**Acceptance Scenarios**:

1. **Given** an indexed repository, **When** a literal search is issued, **Then** results match ripgrep output and return in under 50ms (warm cache).
2. **Given** an indexed repository, **When** a regex search is issued, **Then** the n-gram index narrows candidates before verification, and results are correct.
3. **Given** a query with no extractable n-grams (e.g., `.*`), **Then** the system falls back to full scan gracefully.

---

### User Story 2 - Incremental Index Updates After File Edits (Priority: P1)

An agent edits files during a coding session. The index reflects those changes immediately so subsequent searches find newly written code ("read-your-writes" freshness).

**Why this priority**: Stale indexes cause agents to waste tokens searching for code they just wrote. Freshness is a hard requirement for agent workflows, not a nice-to-have.

**Independent Test**: Edit a file, query for the new content, verify the result appears without a full rebuild.

**Acceptance Scenarios**:

1. **Given** an indexed repo and a file edit, **When** the overlay is updated, **Then** a search for the new content returns the edited file.
2. **Given** multiple rapid edits, **When** searches interleave with edits, **Then** results are always consistent (no partial states).

---

### User Story 3 - Path and File-Type Scoping (Priority: P2)

A user restricts search to specific paths or file types (e.g., only `.rs` files, only `src/`). The path index eliminates non-matching files before content search begins.

**Why this priority**: Path scoping is the cheapest filter and dramatically reduces candidate sets. Agents frequently scope by file type.

**Independent Test**: Run a scoped search and verify only files matching the path/type constraint appear in results.

**Acceptance Scenarios**:

1. **Given** a path scope `src/**/*.rs`, **When** a search is issued, **Then** only Rust files under `src/` are considered.
2. **Given** a file-type filter `-tpy`, **When** a search is issued, **Then** only Python files are searched.

---

### User Story 4 - Symbol-Aware Search (Priority: P3)

A developer searches for a function definition or symbol reference. The system uses a symbol/AST index (tree-sitter or ctags-based) to return precise structural matches rather than raw text hits.

**Why this priority**: Symbol search is a precision layer on top of text search. Valuable for navigation but not required for the core grep-replacement use case.

**Independent Test**: Index a Rust file with known function definitions, query for a symbol, verify only definition sites (not string mentions) are returned.

**Acceptance Scenarios**:

1. **Given** a Rust file with `fn parse_query(...)`, **When** a symbol search for `parse_query` is issued, **Then** the definition location is returned.
2. **Given** a symbol search for an undefined name, **Then** no results are returned.

---

### User Story 5 - Full Index Build from Scratch (Priority: P1)

A user initializes the index for a repository for the first time. The system builds path, content n-gram, and (optionally) symbol indexes. Build completes in under 5 seconds for typical repositories.

**Why this priority**: First-run experience. If initial indexing is too slow, users abandon the tool.

**Independent Test**: Time a full index build on repositories of varying sizes, verify all files are indexed correctly.

**Acceptance Scenarios**:

1. **Given** a repository with 100k LOC, **When** a full index build is triggered, **Then** it completes in under 5 seconds.
2. **Given** a repository with binary files and symlinks, **When** indexing, **Then** binaries are skipped and symlinks handled safely.

---

### Edge Cases

- What happens when a regex pattern causes the n-gram extractor to produce zero grams? (Fallback to full scan.)
- How does the system handle corrupt or truncated index files? (Detect via checksum/magic bytes, rebuild.)
- What happens when a file is deleted between indexing and querying? (Overlay marks deletion, verifier skips.)
- How does the system handle very large files (>10MB)? (Configurable size limit, skip or index with warning.)
- What happens with non-UTF-8 files? (Skip or index as binary with limited search support.)

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST build an n-gram content index from repository files.
- **FR-002**: System MUST build a path/filename index for scope filtering.
- **FR-003**: System MUST decompose regex patterns into n-gram queries (AND/OR) for candidate selection.
- **FR-004**: System MUST verify all candidates against the original pattern (no false positives in results).
- **FR-005**: System MUST support incremental updates via immutable segments + overlay.
- **FR-006**: System MUST use a linear-time regex engine by default to prevent ReDoS.
- **FR-007**: System MUST support literal string search as a fast path.
- **FR-008**: System SHOULD build a symbol/AST index for supported languages (tree-sitter or ctags).
- **FR-009**: System MUST store indexes locally on the user's machine (no server round-trips).
- **FR-010**: System MUST handle concurrent reads without blocking (immutable segments, lock-free reads).
- **FR-011**: System MUST skip binary files and respect .gitignore by default.
- **FR-012**: System SHOULD support sparse n-gram tokenization for improved selectivity.
- **FR-013**: System SHOULD support phrase-aware trigram masks (position + next-char bloom) for adjacency filtering.

### Key Entities

- **Segment**: Immutable index unit containing a gram dictionary and posting lists for a set of files.
- **Posting List**: Sorted list of document IDs (file IDs) associated with an n-gram key.
- **Overlay**: Small mutable segment representing uncommitted file changes on top of base segments.
- **Manifest**: Metadata file tracking active segments, their commit basis, and merge state.
- **Query Plan**: Internal representation of a search decomposed into gram lookups, set operations, and verification steps.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Warm query latency under 50ms (p95) on repositories up to 1M LOC.
- **SC-002**: Full index build under 5 seconds for repositories up to 500k LOC.
- **SC-003**: Incremental update latency under 100ms after a single file edit.
- **SC-004**: Search results are identical to ripgrep for all supported pattern types (correctness).
- **SC-005**: Resident memory under 100MB for the dictionary of a 1M LOC repository.
- **SC-006**: No unsafe Rust without documented justification.

## Assumptions

- Target platform is macOS and Linux (desktop/server). Windows support is deferred.
- The tool runs locally, not as a server. No network protocol needed for v1.
- Repositories are primarily UTF-8 text. Binary file detection and skipping is sufficient.
- Git is available on the host for commit-based snapshot tracking.
- tree-sitter grammars are available as optional dependencies for symbol indexing.
- The Rust `regex` crate is the default verification engine; PCRE2 support is optional/deferred.
