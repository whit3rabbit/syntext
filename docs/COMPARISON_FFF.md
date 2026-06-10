# Comparison: syntext vs fff

Reference: [fff v0.9.4](https://github.com/dmtrKovalenko/fff/tree/v0.9.4) (MIT).
This document records what fff does differently, what syntext adopted from the
comparison, and what it deliberately rejected. Benchmark integration lives in
`docs/BENCHMARKS.md` ("Comparing against fff"); the harness driver is
`scripts/fff_driver.py`.

## What fff is

fff is a file-search **library plus MCP server**, not a CLI. It targets
long-running hosts (editors, AI agents) that keep one process alive across many
searches:

- Resident in-memory index built by a filesystem scan at process start; a
  notify-based watcher applies create/modify/delete events incrementally.
- Sparse compressed **bigram prefilter** over file contents: tracks consecutive
  and skip-1 bigrams, and drops bigrams present in fewer than ~3% or more than
  ~90% of files (low filtering power at either extreme).
- **Frecency ranking** persisted in LMDB: access recency/frequency, git status
  boosts, filename and path-alignment bonuses, distance-from-current-file
  penalties.
- SIMD-accelerated fuzzy matching and path storage (16-byte chunk arena).
- The only runnable binary is `fff-mcp`, an MCP stdio server exposing `grep`,
  `find_files`, and `multi_grep` tools that return ranked, capped results.

## Architectural difference

The core split is where the index lives and who pays for freshness:

| | fff | syntext |
|---|---|---|
| Index lifetime | one process; rebuilt by scan at every start | on-disk segments; built once per repo state, opened by any process |
| Cold start | full filesystem scan + bigram build (~1 s on small repos, more at scale) | `Index::open`: mmap + O(1) structural checks (milliseconds) |
| Warm query | in-process, sub-millisecond | `syntext-persistent` comparable; `syntext-fork` pays process start + open per query |
| Freshness | file watcher, automatic | `st update` / overlay commit, explicit |
| Query language | fuzzy + literal, ranked top-N | literal + regex, exact rg-compatible output |
| Integration | MCP tools, C FFI, language bindings | CLI (`st`), Rust library, WASM |

fff optimizes the resident case and accepts a scan on every process start.
syntext optimizes the stateless-CLI case (every agent invocation is a fresh
process) and accepts an explicit build/update step.

## What fff does better

- **Zero warm-query overhead.** A resident process never re-opens anything;
  syntext-persistent matches this only within one process lifetime.
- **Prefilter selectivity tricks.** Skip-1 bigrams tolerate single-character
  insertions; frequency-band dropping discards bigrams with no filtering
  power. Both are cheap wins for a 2-gram design.
- **Relevance ranking.** Frecency, git-status, and path-alignment scoring
  return *useful* results first, which reduces agent round-trips. syntext
  returns exhaustive, deterministic matches instead.
- **Automatic freshness.** The watcher keeps the index current without a
  user-visible update step.

## What syntext does better

- **Cold start across processes.** The persistent index makes a fresh process
  useful in milliseconds; fff pays a full scan (915 ms on this small repo,
  growing with tree size) per process. For fork-per-query CLI workloads this
  dominates.
- **Regex support** via HIR gram decomposition; fff's grep is literal/fuzzy.
- **Exact rg-compatible counts and output formats** (vimgrep, NDJSON, context
  windows); fff returns ranked top-N content.
- **Integrity model.** Checksummed segments and manifest, 0700 directory
  enforcement, bounds-checked segment parsing, `st verify`.

Measured side by side on this repository (184 tracked files, warm,
`--syntext-search-mode both`): syntext build 375 ms once vs fff
startup-to-ready 1,041 ms per process; warm literal query 0.76 ms
(syntext-persistent) vs 0.32 ms (fff, top-20 ranked) vs 3.4 ms (syntext-fork,
includes process start + open) vs 10.9 ms (rg). Larger-corpus numbers belong in
`docs/BENCHMARKS.md` as they are collected.

## Decisions

### Adopt: cheap warm open

- **Decision:** Make `Index::open` O(1) in postings size: the full `.post`
  checksum became opt-in (`Config::verify_on_open`, `SYNTEXT_VERIFY_ON_OPEN=1`,
  `st verify`), with O(1) structural checks and a manifest-recorded `post_len`
  length check in the default path. Calibration is reused across rebuilds
  (`st index --recalibrate` forces re-measurement).
- **Rationale:** fff's strongest argument against persistent-index designs is
  per-process overhead. Closing the warm-open gap keeps the persistent design
  competitive without adopting a resident process. The security analysis is in
  the `Config::verify_on_open` doc comment: the open-time postings checksum
  was never a query-time integrity control (postings are re-read per query via
  positional reads), and parsing is bounds-checked end to end, so the worst
  case from skipped verification is missing results or `CorruptIndex` errors.

### Adopt: fff as a harness baseline

- **Decision:** `scripts/bench_compare.py` supports `fff` as a tool via an MCP
  stdio driver, reporting startup-to-ready as its build analog and warm grep
  calls against `syntext-persistent`.
- **Rationale:** It is the most relevant agent-facing competitor and keeps the
  persistent-mode numbers honest.

### Reject: resident daemon / watcher mode

- **Decision:** No daemon, no file watcher in v1.
- **Rationale:** `st` mirrors rg's stateless CLI contract; persistence lives on
  disk, not in a process. The overlay + `st update` path covers freshness with
  explicit, testable semantics. Revisit only if an MCP-server mode is added,
  where a resident snapshot would come for free.

### Reject: frecency ranking (v1)

- **Decision:** Output stays exhaustive, deterministic, rg-compatible.
- **Rationale:** Ranked output breaks count parity with rg/grep, which is the
  harness's correctness anchor (SC-004). Relevance ranking belongs to a future
  agent-facing output mode, not the core search contract.

### Reject (v2 candidate): skip-1 bigrams and frequency-band dropping

- **Decision:** No prefilter changes.
- **Rationale:** These compensate for bigrams' weak selectivity; syntext's
  weighted sparse n-grams already target the same problem with longer grams
  and a trained boundary table. Per the selectivity discipline in `CLAUDE.md`,
  any change here requires before/after `cargo bench --bench selectivity`
  evidence first.

## Deferred warm-start items

Assessed during this comparison, not implemented (see `benches/index_open.rs`
for the measurement harness):

- **Sparse `base_doc_to_file_id`** (`src/index/open.rs`): the dense vec is
  only pathological after heavy compaction-induced doc-id sparsity, and its
  consumers sit on hot search paths; a representation change risks query
  regressions for a small cold-open win.
- **Path-index persistence** (`src/index/open.rs`): the O(n log n) path sort
  on open is minor next to the removed checksum I/O; persisting it adds an
  on-disk artifact with invalidation tied to manifest generations.
- **Build batch pipelining** (`src/index/build.rs`): overlapping batch reads
  with segment writes affects build time only and interacts with the
  1.5 GB-per-batch memory budget; needs its own benchmark discipline.
