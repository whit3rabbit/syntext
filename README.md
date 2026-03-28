# ripline

A hybrid code search index for agent workflows, built in Rust. Indexes repositories using sparse n-grams with a pre-trained frequency weight table, then narrows to a small candidate set before verification. Designed as a drop-in replacement for `rg` in AI agent loops where grep is called repeatedly and in parallel.

**Status: under active development.** See [Project status](#project-status) below.

## Why this exists

AI coding agents call grep dozens of times per task. On large monorepos, each `rg` invocation touches every file. Those calls compound into significant stalled agent time per coding session.

ripline builds a content index so queries only touch candidate files, not all files. The verifier confirms matches against actual file content, so results are correct (identical to ripgrep).

## Benchmarks

Real-world benchmark runs are tracked in
[docs/BENCHMARKS.md](docs/BENCHMARKS.md). The table below is
the current snapshot of preset-backed external runs using the shared harness
`scripts/bench_compare.py`.

Method:

- **Note**: These benchmarks were run against `ripline` version `0.01`.
- External repos use the same harness and preset catalog.
- Times below are single-shot preset runs on macOS unless noted otherwise.
- `ripline` search time excludes index build time. Build time is shown separately.
- Linux uses the cheaper shared large-corpus mode (`ripline` + `rg`) because the
  full `ripline` vs `rg` vs `grep` run is too expensive on this machine.

### Index build

| Repo | Preset | Tracked files | Tools | `ripline index` |
|---|---|---:|---|---:|
| Zed Research | `zed_mixed_app` | 3,593 | `ripline`, `rg`, `grep` | `225.103 ms` |
| React | `react_token_aligned` | 6,840 | `ripline`, `rg`, `grep` | `347.956 ms` |
| Rust compiler | `rust_token_aligned` | 58,698 | `ripline`, `rg`, `grep` | `2755.613 ms` |
| TypeScript | `typescript_compiler` | 81,362 | `ripline`, `rg`, `grep` | `3659.839 ms` |
| Node.js | `node_runtime` | 47,364 | `ripline`, `rg`, `grep` | `2810.84 ms` |
| Linux kernel | `linux_token_aligned` | 93,018 | `ripline`, `rg` | `6101.93 ms` |

### Search latency

| Repo | Query | Count match | `ripline` | `rg` | `grep` |
|---|---|---|---:|---:|---:|
| Zed Research | `workspace` | yes | `50.39 ms` | `45.47 ms` | `233.07 ms` |
| Zed Research | `LanguageServerId` | yes | `8.391 ms` | `44.718 ms` | `209.067 ms` |
| Zed Research | `LanguageServer(Id\|InstallationStatus)` | yes | `44.046 ms` | `45.159 ms` | `234.345 ms` |
| React | `useState` | yes | `111.716 ms` | `114.463 ms` | `273.504 ms` |
| React | `getDisplayNameForReactElement` | yes | `10.354 ms` | `110.312 ms` | `315.521 ms` |
| Rust compiler | `rustc_middle` | yes | `107.084 ms` | `1946.141 ms` | `2307.596 ms` |
| Rust compiler | `mir::Body` | yes | `77.344 ms` | `1899.967 ms` | `2049.872 ms` |
| TypeScript | `TransformationContext` | yes | `95.611 ms` | `2573.266 ms` | `3339.558 ms` |
| TypeScript | `NodeBuilderFlags` | yes | `88.656 ms` | `2447.316 ms` | `3270.151 ms` |
| Node.js | `EnvironmentOptions` | yes | `55.626 ms` | `1119.841 ms` | `3550.489 ms` |
| Node.js | `MaybeStackBuffer` | yes | `55.454 ms` | `1089.497 ms` | `3097.734 ms` |
| Linux kernel | `irq_work_queue` | yes | `3220.995 ms` | `3432.32 ms` | `n/a` |
| Linux kernel | `sched_clock` | yes | `125.831 ms` | `3771.862 ms` | `n/a` |
| Linux kernel | `raw_spin_lock` | yes | `121.53 ms` | `3820.238 ms` | `n/a` |

Notes:

- The exact-count validated preset terms are documented in
  [docs/BENCHMARKS.md](docs/BENCHMARKS.md).
- Substring-heavy terms such as `ReactElement`, `useEffect`, and `TyCtxt` are
  intentionally not in the headline README table because they can undercount in
  `ripline` relative to `rg`.
- Historical and exploratory runs, including mismatched-count investigations,
  remain in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

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

## Project status

**Phases 1–6 and 8 complete. Phase 7 (Symbols) and Phase 9 (Polish) in progress.**

See `specs/001-hybrid-code-search-index/tasks.md` for the full implementation plan with 69 tasks across 9 phases.

| Phase | Status | What it delivers |
|---|---|---|
| 1. Setup | ✅ Complete | Cargo project, dependencies, module structure |
| 2. Foundational | ✅ Complete | Weight table, tokenizer, posting lists, correctness harness |
| 3. US5 — Build | ✅ Complete | Full index build from scratch |
| 4. US1 — Search | ✅ Complete | Literal + regex search, ripgrep correctness validation |
| 5. US2 — Incremental | ✅ Complete | Overlay, batch commit, read-your-writes |
| 6. US3 — Path scoping | ✅ Complete | Path/type filters with Roaring bitmaps |
| 7. US4 — Symbols | 🔄 In progress | Tree-sitter symbol extraction, SQLite storage |
| 8. CLI | ✅ Complete | `ripline` binary with grep-compatible output |
| 9. Polish | 🔄 In progress | Benchmarks, edge cases, documentation |

## Design documents

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** -- Quantitative analysis: selectivity math, index size estimates, posting list encoding, design tradeoffs
Detailed specs in `specs/001-hybrid-code-search-index/`:

- **[spec.md](specs/001-hybrid-code-search-index/spec.md)** -- Feature specification with user stories and acceptance criteria
- **[research.md](specs/001-hybrid-code-search-index/research.md)** -- 19-section architecture research covering every subsystem
- **[data-model.md](specs/001-hybrid-code-search-index/data-model.md)** -- Entity definitions and relationships
- **[contracts/](specs/001-hybrid-code-search-index/contracts/)** -- Library API, CLI, and segment format contracts
- **[tasks.md](specs/001-hybrid-code-search-index/tasks.md)** -- Implementation plan with dependency graph

## License

MIT
