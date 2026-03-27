# Implementation Plan: Hybrid Code Search Index

**Branch**: `001-hybrid-code-search-index` | **Date**: 2026-03-25 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `/specs/001-hybrid-code-search-index/spec.md`

## Summary

Build a Rust library and CLI tool that indexes repository files using sparse n-gram content indexes (with pre-trained frequency weight table and lowercase normalization), Roaring bitmap path indexes, and optional Tree-sitter symbol indexes. A query router classifies patterns into literal (memchr fast path), indexed regex (HIR decomposition to gram tree), or full scan. Posting list intersection uses adaptive merge/gallop with early termination. Index layout uses immutable mmap-friendly segments with a single merged in-memory overlay (ArcSwap snapshot isolation) for incremental updates. Batched-segment build (256MB per batch) bounds memory during construction.

## Technical Context

**Language/Version**: Rust 1.75+ (2021 edition)
**Primary Dependencies**: regex + memchr (verification), memmap2 (mmap segments), tree-sitter (optional symbol indexing), ignore (gitignore/file-type filtering), roaring (bitmap posting lists), rayon (parallel build), zerocopy (segment format), arc-swap (snapshot isolation)
**Storage**: Custom immutable segment files (mmap dictionary + sequential postings), no embedded database for core index
**Testing**: cargo test (unit + integration), criterion (benchmarks)
**Target Platform**: macOS, Linux (desktop/local)
**Project Type**: library + CLI
**Performance Goals**: sub-50ms warm queries (p95), sub-5s full index build for typical repos, sub-100ms incremental updates
**Constraints**: <100MB resident memory for dictionary, linear-time regex only by default, no unsafe without justification
**Scale/Scope**: Repositories up to 1M+ LOC, thousands of files

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Security First | PASS | Linear-time regex by default (FR-006), input validation at boundaries (file paths, patterns), no unsafe without justification (SC-006), mmap with bounds checks |
| II. Speed-Optimized Development | PASS | Core design targets sub-50ms queries via index candidate selection, sparse n-grams reduce posting lookups, mmap-friendly layout for zero-copy reads |
| III. Research Before Action | PASS | PLAN.md + research.md cover prior art (Cursor, GitHub Blackbird, Zoekt, Tantivy, ripgrep). All dependency choices audited. |
| IV. Test and Document Everything | PASS | Plan includes unit tests (tokenizers, posting ops, query planning), integration tests (index build + query), benchmarks (criterion). Doc comments for public API. |
| V. Enforce Module Size Limits | PASS | Architecture splits into focused modules: tokenizer, posting lists, segments, query planner, verifier, path index, symbol index. No module should exceed 400 lines. |

**Post-Phase 1 re-check**: All principles still PASS. Segment format uses xxhash64 checksum + magic byte validation (Security). Sparse n-grams minimize posting lookups (Speed). All decisions documented in research.md with rationale (Research). Test structure defined in project layout (Test). Module count is 12 files across 7 directories, all well under 400 lines (Size Limits). No violations.

## Project Structure

### Documentation (this feature)

```text
specs/001-hybrid-code-search-index/
├── plan.md              # This file
├── spec.md              # Feature specification
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
└── tasks.md             # Phase 2 output (speckit.tasks)
```

### Source Code (repository root)

```text
src/
├── lib.rs               # Public API surface (Index, Config, SearchOptions, etc.)
├── tokenizer/
│   ├── mod.rs           # Tokenizer trait + sparse n-gram impl
│   └── weights.rs       # Character-pair frequency weights ([u16; 65536])
├── index/
│   ├── mod.rs           # Index builder + reader coordination
│   ├── segment.rs       # Immutable segment: dictionary + postings file (RPLX format)
│   ├── overlay.rs       # Generation-based overlay with batch commit
│   ├── manifest.rs      # Segment manifest + atomic write-then-rename
│   └── merge.rs         # Background segment compaction
├── posting/
│   ├── mod.rs           # Posting list types + intersection/union ops
│   └── roaring.rs       # Roaring bitmap fallback for dense terms (>8K entries)
├── query/
│   ├── mod.rs           # Query router (literal / indexed regex / full scan)
│   ├── regex_decompose.rs  # HIR walker -> GramQuery tree
│   └── planner.rs       # Cardinality-based intersection ordering
├── search/
│   ├── mod.rs           # Search executor: route -> candidates -> verify
│   └── verifier.rs      # Tiered: memchr::memmem for literals, regex for patterns
├── path/
│   ├── mod.rs           # Path index: component-based Roaring bitmap sets
│   └── filter.rs        # Glob/type scope filters
├── symbol/
│   ├── mod.rs           # Symbol index (Tree-sitter + SQLite, separate mode in v1)
│   └── extractor.rs     # Symbol extraction from parse trees
└── cli/
    └── mod.rs           # CLI entry point (clap)

tests/
├── integration/
│   ├── index_build.rs   # Full index build + query correctness
│   ├── incremental.rs   # Overlay batch commit + compaction correctness
│   ├── correctness.rs   # Comparison against ripgrep baseline
│   └── selectivity.rs   # Candidate set size measurements
└── unit/
    ├── tokenizer.rs     # Sparse n-gram tokenization tests
    ├── posting.rs       # Posting list operation tests
    ├── query.rs         # Query decomposition + routing tests
    └── overlay.rs       # Batch commit atomicity tests

benches/
├── query_latency.rs     # Decomposed per-phase benchmarks (dict, posting, intersection, verify)
├── selectivity.rs       # Candidate set size vs pattern type
└── index_build.rs       # Index build throughput benchmarks
```

**Structure Decision**: Single project (library + CLI binary in one crate). The library exposes the public API; the CLI binary is a thin wrapper. This avoids workspace overhead while the project is small. If the CLI grows significantly, extract to a workspace later.

## Complexity Tracking

No violations to justify.
