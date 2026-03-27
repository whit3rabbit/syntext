# ripline

A hybrid code search index for agent workflows, built in Rust. Indexes repositories using sparse n-grams with a pre-trained frequency weight table, then narrows to a small candidate set before verification. Designed as a drop-in replacement for `rg` in AI agent loops where grep is called repeatedly and in parallel.

**Status: under active development.** See [Project status](#project-status) below.

## Why this exists

AI coding agents call grep dozens of times per task. On large monorepos, each `rg` invocation touches every file. Those calls compound into significant stalled agent time per coding session.

ripline builds a content index so queries only touch candidate files, not all files. The verifier confirms matches against actual file content, so results are correct (identical to ripgrep).

## Architecture

For the full quantitative analysis (selectivity math, index size estimates, posting list encoding tradeoffs), see **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)**.

The high-level flow:

```
Query -> Router -> [Literal | Indexed Regex | Full Scan]
                        |
                   Gram extraction
                        |
                   Posting list intersection (smallest-first)
                        |
                   Candidate file IDs
                        |
                   Verifier (memchr or regex against file content)
                        |
                   Results
```

Three index components feed candidate selection:

- **Content index**: sparse n-gram posting lists (the core). Trigram augmentation ensures no false negatives for token-aligned queries.
- **Path index**: Roaring bitmap component sets for path/type filtering.
- **Symbol index** (optional): Tree-sitter extraction into SQLite.

Segments are immutable single-file mmap structures (RPLX format). Updates go through an in-memory overlay with atomic batch commit via `ArcSwap`.

## Benchmarks

No benchmark data yet. Tables will be populated once the search pipeline is complete and measured on real corpora. Until then, the correctness test suite (ripgrep oracle) is the primary validation.

## Usage

### CLI

```bash
# Build the index
ripline index --stats

# Search
ripline search "fn parse_query"          # literal
ripline search "fn\s+\w+_query"          # regex
ripline search -l "parse_query("         # explicit literal (pattern has metachar)
ripline search -i "parsequery"           # case-insensitive
ripline search -t rs "impl.*Iterator"    # restrict to Rust files
ripline search --json "TODO"             # JSON output for tooling

# Incremental update after edits
ripline update

# Status
ripline status
```

### Library

```rust
use ripline::{Config, Index, SearchOptions};

let config = Config {
    repo_root: "/path/to/repo".into(),
    index_dir: "/path/to/repo/.ripline".into(),
    ..Config::default()
};

let index = Index::open(config)?;
index.build()?;

// Search
let results = index.search("fn parse_query", &SearchOptions::default())?;

// Agent workflow: edit files, then search
index.notify_change(Path::new("src/foo.rs"))?;
index.notify_change(Path::new("src/bar.rs"))?;
index.commit_batch()?;  // atomic visibility
let fresh_results = index.search("new_function", &SearchOptions::default())?;
```

## Project status

**Phase 2 (Foundational) — in progress.** The weight table is generated, the tokenizer is implemented, the correctness test harness is built. Posting list operations and the segment format are next.

See `specs/001-hybrid-code-search-index/tasks.md` for the full implementation plan with 69 tasks across 9 phases.

| Phase | Status | What it delivers |
|---|---|---|
| 1. Setup | ✅ Complete | Cargo project, dependencies, module structure |
| 2. Foundational | 🔧 In progress | Weight table, tokenizer, posting lists, correctness harness |
| 3. US5 — Build | Not started | Full index build from scratch |
| 4. US1 — Search | Not started | Literal + regex search, ripgrep correctness validation |
| 5. US2 — Incremental | Not started | Overlay, batch commit, read-your-writes |
| 6. US3 — Path scoping | Not started | Path/type filters with Roaring bitmaps |
| 7. US4 — Symbols | Not started | Tree-sitter symbol extraction, SQLite storage |
| 8. CLI | Not started | `ripline` binary with grep-compatible output |
| 9. Polish | Not started | Benchmarks, edge cases, documentation |

## Design documents

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** -- Quantitative analysis: selectivity math, index size estimates, posting list encoding, design tradeoffs
- **[docs/KNOWN_ISSUES.md](docs/KNOWN_ISSUES.md)** -- Open and resolved design issues

Detailed specs in `specs/001-hybrid-code-search-index/`:

- **[spec.md](specs/001-hybrid-code-search-index/spec.md)** -- Feature specification with user stories and acceptance criteria
- **[research.md](specs/001-hybrid-code-search-index/research.md)** -- 19-section architecture research covering every subsystem
- **[data-model.md](specs/001-hybrid-code-search-index/data-model.md)** -- Entity definitions and relationships
- **[contracts/](specs/001-hybrid-code-search-index/contracts/)** -- Library API, CLI, and segment format contracts
- **[tasks.md](specs/001-hybrid-code-search-index/tasks.md)** -- Implementation plan with dependency graph

## License

MIT