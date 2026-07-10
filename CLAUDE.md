# syntext

Hybrid code search index for agent workflows. Sparse n-gram content index + Roaring bitmap path index + optional Tree-sitter symbol index.

> **Crate name:** `syntext`. Binary: `st`.
> **Upstream:** https://github.com/whit3rabbit/syntext.git

- See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for quantitative design reasoning.

## Architecture

- **Segment format**: immutable single-file segments (SNTX magic, TOC footer, page-aligned dictionary). Postings are delta-varint or Roaring bitmap depending on list size (threshold: 8K entries).
- **Tokenizer**: two-tier boundary detection (forced boundaries at code delimiters + weight-based within alphanumeric spans). Lowercase normalization at index/query time. Weight table lives in `src/tokenizer/weights.rs`. Forced boundary set in `is_forced_boundary()` in `src/tokenizer/mod.rs`.
- **Query router**: literal (`build_covering` + memchr::memmem) / indexed regex (`build_covering_inner` + HIR decomposition) / full scan. Cardinality-based intersection ordering in `query/mod.rs`. Fallback skips index when smallest posting list > 10% of total docs. Path filter always first.
- **Overlay**: single merged in-memory OverlayView, rebuilt from all dirty files on each `commit_batch()`. ArcSwap<IndexSnapshot> for snapshot isolation. On-disk generations for crash recovery only.
- **Build**: batched-segment construction, 256MB per batch, sort-based aggregation, rayon for parallelism. Peak memory ~1.5GB per batch.

## General Rules

- **Never put full file paths in documents.** Always use relative paths when referencing internal files. When referencing external repositories, use links to the original Git repository and lock it to a specific version or tag when possible.
- **Windows Compatibility & Testing**:
    - **Explicitly `drop(index)`**: Always call `drop(index)` at the end of tests that use `Index::open` or `Index::build`. Windows prevents deleting temporary directories or renaming files if file handles (locks) or memory maps are still active.
    - **Avoid `io::Error::other`**: Use `io::Error::new(io::ErrorKind::Other, ...)` for compatibility with Rust versions < 1.74.
    - **Directory `sync_all`**: Do not call `sync_all()` on directory handles on Windows; it is not supported and returns `Access is denied`. Use `#[cfg(not(windows))]`.
    - **Git Binary**: Use platform-aware resolution (searching for `git.exe` on Windows) in `helpers.rs`.
    - **Forward-slash path normalization**: All paths stored in segments must use forward slashes (`/`). Use `path_util::normalize_to_forward_slashes()` at ingestion boundaries. Byte-level matching in `path/filter.rs` and `path/mod.rs` splits on `b'/'` only.
    - **Illegal filename characters in tests**: Windows forbids `\`, `<`, `>`, `"`, `|`, `?`, `*`, and control characters (including `\t`, `\n`) in file/directory names. Gate such tests with `#[cfg(unix)]`.
    - **`Index::build` in a git repo indexes `.git/hooks/*.sample`**, so `base_doc_count` is ~20+, not your fixture file count. Tests that need `OverlayFull` (overlay > 50% of base) must size the change set off `index.stats().total_documents`, not a hardcoded small number.

## Dependencies

| Crate | Purpose |
|---|---|
| regex, regex-syntax | verification engine + HIR walking for gram decomposition |
| memchr | literal verification fast path (memmem) |
| memmap2 | mmap segment files |
| roaring | bitmap posting lists (dense terms), path index component sets |
| rayon | parallel index build |
| arc-swap | lock-free snapshot swapping for concurrent reads |
| ignore | .gitignore respect, file-type filtering |
| clap | CLI (with derive) |
| serde, serde_json | manifest serialization |
| uuid | segment IDs (v4) |
| xxhash-rust | checksums (xxh64) |
| wasm-bindgen, js-sys | WASM bindings (`wasm` feature only) |
| tree-sitter | optional symbol extraction (Tier 1 languages) |
| rusqlite | optional symbol index storage (bundled, behind `symbols` feature) |
| criterion | benchmarks (dev-only) |
| proptest | property-based coverage invariant tests (dev-only) |
| tempfile | temporary dirs in tests (dev-only) |

## Feature Flags

The crate supports the following Cargo feature flags:

* `default`: Enables the `cli` feature (command-line interface binary and native library support).
* `cli`: Builds the command-line binary `st`. Depends on `native` and the optional `clap` dependency.
* `native`: Native library support. Includes `memmap2`, `rayon`, `fs2`, and `ignore` (enables filesystem access, multi-threading, and Git integrations).
* `wasm`: WASM target support. Enables `wasm-bindgen` API, fully in-memory index, and disables all native filesystem and threading dependencies.
* `symbols`: Tree-sitter symbol extraction and SQLite local cache storage support.

## Implementation Order

These are non-negotiable ordering constraints:

1. **Weight table first** (`src/tokenizer/weights.rs`). Train from real GitHub source (Rust, Python, TypeScript, Go, Java). Every module depends on good grams. Validate by eyeballing grams on real files.
2. **Ripgrep correctness test before index**. Fixture repo with edge cases (unicode, empty files, whitespace-only, long lines, binary-looking text). Run ripgrep for expected output. SC-004: results identical to ripgrep.
3. **HIR walker edge case**: `(foo)?bar` correctly produces `Grams("bar")` only, not `And(Grams("foo"), Grams("bar"))`. This is correct (pattern matches `bar` alone). Do not "fix" this. Test it explicitly.

## WASM build

```sh
# Type-check only (fast)
cargo check --target wasm32-unknown-unknown --features wasm --no-default-features

# Full build (requires wasm-pack)
cargo install wasm-pack
wasm-pack build --target bundler -- --features wasm --no-default-features
# output: pkg/  (syntext_bg.wasm, syntext.js, syntext.d.ts, package.json)

# Other targets
wasm-pack build --target nodejs  -- --features wasm --no-default-features
wasm-pack build --target web     -- --features wasm --no-default-features
```

The `wasm` feature enables `WasmIndex` in `src/wasm.rs` via `src/index/wasm_index.rs`.
All documents live in the overlay (no disk writes, no mmap, no locking).
The `wasm32-unknown-unknown` target is also added in `release.yml` as a `build-wasm` job.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
```

See `install.sh` at the repo root for platform detection logic (macOS: Homebrew cask first; Linux: `.deb` on Debian/Ubuntu amd64, raw binary otherwise).

## Releasing

When cutting a release, bump the version in all of these together:

- `Cargo.toml` `version` (and regenerate `Cargo.lock` with any cargo command).
- `install.sh` `SYNTEXT_VERSION` default (and the comment on the line above it).
  Update it once the tag's release artifacts are published, since the installer
  downloads `v${SYNTEXT_VERSION}` binaries; pointing it at a version with no
  published binaries breaks `curl | sh` installs.
- `CHANGELOG.md`: move `[Unreleased]` to the new `[x.y.z] - YYYY-MM-DD` section.

Then commit (`release: vX.Y.Z`), tag `vX.Y.Z`, and push both the branch and the
tag. The tag triggers `release.yml` to build and publish binaries.

## Commands

```
cargo test                    # unit + integration tests
cargo clippy                  # lint, must pass with no warnings
cargo bench                   # criterion benchmarks
SYNTEXT_LOG_SELECTIVITY=1 cargo test --test correctness -- --nocapture
                              # show per-query selectivity stats
cargo test --test tokenizer   # unit: tokenizer only
cargo test --test posting     # unit: posting lists only
cargo test --test query       # unit: query router only
cargo test --test overlay     # unit: overlay/snapshot only
cargo test --test boundary_fuzz  # unit: boundary fuzzing
cargo test --test index_build # integration: index construction
cargo test --test incremental # integration: incremental updates
cargo test --test symbols     # integration: symbol index
cargo test --test oracle_self --features oracle  # integration: self-differential oracle
cargo test --test oracle_incremental --features oracle # integration: incremental overlay differential oracle
cargo test --test oracle_cli --features oracle   # integration: CLI subprocess differential oracle
cargo test --features oracle                     # run all tests including oracle targets
cargo bench --bench query_latency -- --sample-size 10
cargo bench --bench index_build -- --sample-size 10
cargo bench --bench selectivity -- --sample-size 10
cargo bench --bench freshness -- --sample-size 10
```

## Differential Oracle (st ↔ rg)

To ensure search correctness, prevent false-negatives/positives, and maintain robust query routing, `syntext` integrates a differential testing framework comparing its outputs directly against `ripgrep` (`rg`).

- **Oracle Version**: Locked to `ripgrep 15.1.0` (specified in `tests/oracle/ORACLE_VERSION`).
- **Divergence Policy**: Allowed/intentional differences (e.g. smart-case handling, result ordering, and `-v` candidate-only scope filter) are detailed in `tests/oracle/DIVERGENCES.md` and parsed/normalized gracefully during comparisons.
- **Self-Differential Test (`oracle_self`)**: An in-process `proptest`-based target comparing the standard routed query match results against forced full-scan results.
- **CLI Subprocess Differential Test (`oracle_cli`)**: A subprocess-based target executing CLI runs of `st` against `rg` on dynamically generated corpora.
- **Current Status**: Fully implemented, integrated into the quality gate, and running cleanly. All differential targets pass with zero failures.
- **proptest randomness**: oracle targets are random per run — one green run is a single sample, NOT proof. Failures persist to `tests/integration/*.proptest-regressions` (deterministic replay next run; check them in). Sweep with `PROPTEST_CASES=1024 cargo test --features oracle <name>` before trusting a change.
- **`oracle_incremental` gotcha**: it drives a subprocess `st` that auto-updates from git independently; the harness must NOT hold a concurrent in-process `Index` lock during the subprocess search, or the subprocess's `OverlayFull` rebuild is blocked (LockConflict → false-stale Tier-A failure). Release/reopen around the subprocess.


## Benchmarking Reference

Run Criterion benches **sequentially**, one benchmark target at a time. Do not
run multiple bench targets in parallel, and do not use one shell command that
launches several Criterion benches back-to-back when collecting numbers for docs.

### Synthetic corpus benchmarks

```sh
cargo bench --bench query_latency -- --sample-size 10
cargo bench --bench index_build -- --sample-size 10
cargo bench --bench selectivity -- --sample-size 10
cargo bench --bench freshness -- --sample-size 10
```

Use these when changing tokenizer coverage, posting execution, query routing,
or commit-path performance. Record before/after results in
`docs/BENCHMARKS.md`. `freshness` isolates `detect_changed_files` and the
bounded `Index::update_from_git` apply path (see `benches/freshness.rs`) from
the full 100k-file `open_search_e2e` nightly target; use it when changing
incremental freshness detection or the `update_from_git` bounded-apply cost.

### External repository comparison harness

Use the Python harness for `syntext` vs `rg` vs `grep` on a real repo:

```sh
python3 scripts/bench_compare.py --preset react_token_aligned
python3 scripts/bench_compare.py --repo /path/to/repo --query literal:symbol
```

Key flags: `--build-only` (index build timing only), `--syntext-search-mode both` (cold + hot),
`--build-iterations 1 --search-iterations 1 --warmups 0` (quick pass on large repos),
`--json`, `--markdown-table-only --output /tmp/out.md`. Full option list: `--help`.

Use the preset catalog in [`benchmarks/repo_presets.json`](benchmarks/repo_presets.json)
and the rationale in [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).
The benchmark process should stay on this one script and this one preset catalog
so runs are reproducible over time.

### macOS corpus setup

Shallow clones are enough for search benchmarks:

```sh
mkdir -p ./_syntext-bench
git clone --depth 1 -b v6.8 https://github.com/torvalds/linux.git ./_syntext-bench/linux
git clone --depth 1 -b 1.77.0 https://github.com/rust-lang/rust.git ./_syntext-bench/rust
git clone --depth 1 -b v18.2.0 https://github.com/facebook/react.git ./_syntext-bench/react
git clone --depth 1 -b v5.4.3 https://github.com/microsoft/TypeScript.git ./_syntext-bench/typescript
git clone --depth 1 -b v20.12.0 https://github.com/nodejs/node.git ./_syntext-bench/node
```

macOS warning: default APFS is case-insensitive. `linux` and `rust` have
case-colliding paths, so Git will warn and only one path from each collision set
will exist in the working tree. That is acceptable for rough performance work,
but not for strict corpus-correctness claims.

### Recommended preset runs

```sh
python3 scripts/bench_compare.py --preset zed_mixed_app
python3 scripts/bench_compare.py --preset react_token_aligned
python3 scripts/bench_compare.py --preset rust_token_aligned
python3 scripts/bench_compare.py --preset linux_token_aligned
python3 scripts/bench_compare.py --preset typescript_compiler
python3 scripts/bench_compare.py --preset node_runtime
```

### Benchmark query rules

- Prefer token-aligned literal queries. Current `syntext` coverage guarantees are
  strongest there.
- Do not use substring-heavy literals like `ReactElement`, `useEffect`, or
  `TyCtxt` as headline benchmark terms unless exact count agreement has already
  been verified.
- If `syntext`, `rg`, and `grep` counts differ, treat the timing comparison as
  suspect until the mismatch is explained.

## Weight Table Generation

`src/tokenizer/weights.rs` is auto-generated by `scripts/weights_gen.py`. Do not edit by hand.

### One-time setup

```sh
python3 -m venv .venv
.venv/bin/pip install datasets numpy
```

HuggingFace access is required for the default corpus. Request access at
https://huggingface.co/datasets/bigcode/the-stack-smol, then authenticate:

```sh
hf auth login   # or: huggingface-cli login
```

### Generate from HuggingFace (recommended, ~175MB download to /tmp)

```sh
HF_DATASETS_CACHE=/tmp/hf_cache .venv/bin/python scripts/weights_gen.py \
  --output src/tokenizer/weights.rs
```

Corpus: Rust, Python, JS, Go, Java, C, TypeScript (~25MB each, ~175MB total).
`cpp` is missing from the-stack-smol; that is expected and non-fatal.

### Generate from local code directories

```sh
.venv/bin/python scripts/weights_gen.py \
  --source local \
  --dirs ~/projects/linux ~/projects/rustc ~/projects/cpython \
  --output src/tokenizer/weights.rs
```

### Verify the output

The script prints diagnostics after generation. A correct table shows:
- `'  '` (double space), `'re'`, `'er'`: weight < 12000 (common, gram interior)
- `'qz'`, `'xj'`: weight > 28000 (rare, good gram boundaries)
- Unseen pairs: weight 65535
- Non-zero pairs: ~16,000+ (see CLAUDE.local.md for last-run count)

If all weights are 65535, the script ran with no corpus. Check HF auth and dataset access.

## Quality Gates

All PRs must pass before merge:

1. `cargo test` -- no failures
2. `cargo clippy` -- no warnings
3. No source file > 400 lines (test files exempt)
   - Inline `#[cfg(test)] mod tests` counts toward the total. Over 400 mostly from tests: extract to a sibling via `#[cfg(test)] #[path = "X_tests.rs"] mod tests;` (pattern: `overlay_tests.rs`, `freshness_tests.rs`). Over 400 from real code: split into a child module — a `mod` under `index`/`cli` can call the parent type's private methods (Rust descendant privacy), so no `pub` widening; add `pub(super)` only when a helper a parent/sibling calls moves into the child (e.g. `update.rs`, `path_resolve.rs`).
4. New public APIs have doc comments
5. No new `unsafe` without documented justification
6. Performance changes include benchmark results (before/after)

## Key Design Decisions

- **No probabilistic masks** (Cursor's locMask/nextMask). 8-bit masks saturate after ~12 occurrences. Sparse n-grams provide better selectivity.
- **No FM-index for v1**. Construction time 10x slower, locate is expensive, zero incrementality. Valid v2 path.
- **No content-defined chunking for v1**. Most files are small, posting list inflation outweighs gains. Block-level positional data is the preferred v2 alternative.
- **Lowercase normalization** at index time. ~15-20% more candidates for case-sensitive queries, eliminated by verifier. Dual dictionary is a v2 option.
- **Stable file IDs are a long-term choice** for incremental path-index maintenance, even though the first implementation regressed `commit_batch`; compare against `docs/BENCHMARKS.md` before changing course.
- **File-level documents**, not chunks. Segment format uses u32 IDs that can represent chunk_ids later.
- **Full reindex at 30% overlay threshold** is the only mechanism that cleans stale doc_ids from base segments. Overlay compaction is not needed (single merged view).
- **Overlay is in-memory only.** `commit_batch` persists nothing to disk (`open.rs` loads `OverlayView::empty()`); only `build::build_index` and compaction write durable segments. Any cross-process freshness (git hooks, async catch-up -- both run as a separate `st update` process) must full-rebuild, not overlay-apply, or a later `st search` process won't see the change. Incremental-per-commit needs the deferred overlay-generation persistence first.

## Project Structure

```
src/
  lib.rs                      # public API (Index, Config, SearchOptions)
  main.rs                     # binary entry point (st); empty stub on wasm32
  wasm.rs                     # wasm-bindgen WasmIndex public API (wasm feature only)
  base64.rs                   # base64 encoding helpers
  git_util.rs                 # git binary resolution + path safety (shared by CLI and index)
  path_util.rs                # path normalization utilities
  tokenizer/
    mod.rs                    # sparse n-gram extraction (build_all, forced boundaries)
    covering.rs               # build_covering and build_covering_inner (query gram extraction)
    weights.rs                # pre-trained [u16; 65536] byte-pair frequency table
    tests.rs                  # unit tests for tokenizer
  index/
    mod.rs                    # Index struct, top-level re-exports, search_fresh (bounded update + search)
    tests.rs                  # unit tests for Index (path resolution, compaction, overlay)
    open.rs                   # open / open_inner entry points
    commit.rs                 # commit_batch logic
    helpers.rs                # free functions shared across index modules
    build.rs                  # build pipeline (calibrate_threshold, build_index)
    compact.rs                # compaction execution
    compact_tests.rs          # unit tests for compaction
    compact_plan.rs           # compaction planning types, plan / forced_plan
    encoding.rs               # varint / posting encoding helpers
    io_util.rs                # secure file-open helpers (O_NOFOLLOW, inode check)
    snapshot.rs               # BaseSegments, IndexSnapshot, new_snapshot
    segment/
      mod.rs                  # SNTX segment format constants, DocEntry, MmapSegment
      tests.rs                # unit tests for segment round-trips and security checks
      open.rs                 # MmapSegment constructors: from_bytes, open (v2), open_split (v3)
      reader.rs               # MmapSegment::open and open_split (mmap read path)
      segment_writer.rs       # SegmentWriter (serialize to SNTX)
    overlay.rs                # OverlayView + ArcSwap<IndexSnapshot>
    overlay_tests.rs          # unit tests for overlay incremental builds
    manifest.rs               # manifest.json + atomic write-then-rename
    manifest_tests.rs         # unit tests for manifest serialization and GC
    pending.rs                # PendingEdits buffer for incremental updates
    stats.rs                  # index statistics computation
    walk.rs                   # directory walking / file discovery
    wasm_index.rs             # InMemoryIndex for wasm32 (no disk I/O, wasm feature only)
  posting/
    mod.rs                    # posting list types + adaptive intersection/union
    roaring_util.rs           # Roaring bitmap integration for dense terms
  query/
    mod.rs                    # query router + cardinality-based intersection ordering
    regex_decompose.rs        # HIR walker -> GramQuery tree
  search/
    mod.rs                    # search executor
    tests.rs                  # unit tests for search routing and selectivity
    lines.rs                  # line extraction for context rendering
    executor.rs               # query execution against base segments + overlay
    resolver.rs               # doc_id -> path + content resolver
    verifier.rs               # tiered: memchr for literals, regex for patterns
  path/
    mod.rs                    # Roaring bitmap component index
    filter.rs                 # glob/type scope filters
  symbol/
    mod.rs                    # Tree-sitter + SQLite symbol index
    extractor.rs              # symbol extraction from parse trees
  cli/
    mod.rs                    # clap CLI entry point
    tests.rs                  # unit tests for CLI parsing and command dispatch
    args.rs                   # Cli struct and ManageCommand enum (flag definitions)
    commands.rs               # management subcommand definitions
    scope.rs                  # path-scope filtering: glob matching, --files mode, deduplication
    bench.rs                  # hidden bench-search subcommand (in-process latency)
    git_resolve.rs            # git binary resolution + path safety helpers
    fallback.rs               # opt-in rg/grep fallback on missing index (--fallback / SYNTEXT_FALLBACK_RG)
    manage.rs                 # index/status/update subcommand handlers
    render/
      mod.rs                  # shared utilities, re-exports, JSON helpers, flat/heading/vimgrep
      context.rs              # context-window rendering (before/after lines)
      count.rs                # per-file match count rendering
      invert.rs               # corpus-wide invert match (st -v), walks all scoped paths
      json.rs                 # NDJSON output (rg-compatible begin/match/context/end/summary)
      only_matching.rs        # only-matching substring rendering
    search.rs                 # search arg parsing, query execution, result dispatch
```

## Spec Location

All design documents are in `docs/`. When in doubt about a design decision, check `docs/ARCHITECTURE.md` first -- it covers every major subsystem with Decision / Rationale / Alternatives Considered.

## 2026-07-09: Eatahorse run -- Bounded Update-on-Search (hardening + git hooks) -- COMPLETE

Board: `.eatahorse-task-list-bounded-update-on-search-compl`. First pass hit a
40-iteration cap mid-card (EH-0009 in `doing`, EH-0010..EH-0018 unstarted); a
second pass on the same board finished the rest. Final: 77 iterations total,
all 18 cards `done`, 0 blocked, 0 dropped, stopReason `board cleared`. As of
this write-back none of the work below is committed -- it is all sitting in
the working tree (`git status` shows the modified/new files listed under
"Project Structure" below).

**Goal:** the auto-update-on-search mechanism (`freshness.rs`, `UpdateLimits`,
`Index::update_from_git`, `cmd_search`'s `auto_update_budget_ms` /
`auto_update_max_files`) already existed; this board hardens it (Phase 0-1),
adds the two missing pillars -- git hooks and open-cost reduction (Phase 2, 4)
-- then wires in fsmonitor, a library API, and the differential oracle.

**Completed (EH-0001..EH-0017, all done):**
- Fixed change-set dedup and symlink misclassification in `update_from_git`.
- Proved auto-update never changes search's exit code/stdout, and that a
  requeue survives a failed background update.
- Completed the staleness-notice + async-catchup contract on search (this
  logic now lives in `src/cli/catchup.rs`, split out of `src/cli/search.rs`
  to stay under the 400-line quality-gate limit -- if you grep for the
  staleness-notice string, look in `catchup.rs`, not `search.rs`).
- Routed `--files` and invert (`st -v`) scoped listing through bounded update.
- Added a `files_behind` freshness field to `st status`.
- Added a githooks vendor (`src/hook/vendors/githooks.rs`: install/uninstall
  for post-commit/checkout/merge/rewrite), made it worktree-correct with
  defined no-index behavior, and wired it into the installer surface and
  `st init`.
- Persisted `PathIndex` to a checksummed `paths.idx` sidecar
  (`src/index/paths_idx.rs`), written by `build.rs`/`compact.rs`, loaded by
  `open.rs` with fallback to the existing rebuild-from-segments path on any
  read/checksum failure -- this is what makes the per-search bounded
  `update_from_git` reopen path cheap (it no longer rebuilds `PathIndex` from
  every doc entry in every base segment on every search).
- Made the dictionary checksum opt-in and closed the SIGBUS window by
  switching dictionary/doc-table reads to `pread` instead of the mmap slice
  (`src/index/segment/dict_read.rs`, extracted from `segment/mod.rs` to stay
  under the 400-line limit).
- Added freshness integration tests (`tests/integration/incremental.rs`) and
  persisted/lazily-initialized `base_doc_to_file_id` (touches
  `src/index/{open,snapshot,compact_tests,tests}.rs`, `src/search/mod.rs`,
  `src/index/wasm_index.rs`).
- Added a 100k-file open+detect+search bench gate
  (`benches/open_search_e2e.rs`) wired into `.github/workflows/nightly.yml`
  as its own job (too slow for PR CI), plus a separate, cheap
  `benches/freshness.rs` (`[[bench]] name = "freshness"` in `Cargo.toml`,
  see `## Commands`/`## Benchmarking Reference`) for per-PR-sized freshness
  timing; baseline recorded in `docs/BENCHMARKS.md` ("bench-freshness
  baseline, 2000 synthetic files (2026-07-09)").
- Added the fsmonitor detection hint (`maybe_print_fsmonitor_tip`) and
  `st init --fsmonitor` opt-in (`enable_fsmonitor`/`is_fsmonitor_enabled` in
  `freshness.rs`, `cmd_init_fsmonitor` in `src/cli/mod.rs`) -- confirmed
  these were actually implemented in an earlier pass and only needed the
  bench-freshness half finished.
- Added `Index::search_fresh` (`src/index/mod.rs`) as the one-call
  "bounded-update-then-search" library API, and re-exported `ChangeSet`,
  `FreshnessError`, `UpdateOutcome`, `UpdateLimits` at `syntext::index::{..}`
  (additive `pub use`, the old `index::freshness::` path still resolves).
- Encoded the staleness-invariant pair in the differential oracle
  (`tests/integration/oracle_freshness.rs`, `required-features = ["oracle"]`):
  in one test run, a stale `search_fresh` call (forced `TooManyFiles` via a
  tiny `UpdateLimits`) must match `rg` on the *pre*-mutation tree, and a
  generous-limits `search_fresh` call on the same index must match `rg` on
  the *post*-mutation tree. Key finding baked into the test design:
  `resolve_doc` (`src/search/resolver.rs`) re-reads live file bytes for
  already-indexed base-segment docs regardless of posting-list staleness, so
  the stale-half mutation must be a brand-new *untracked* file (not an edit
  to an existing file) or the test would silently pass against live bytes
  instead of the pre-mutation snapshot.
- Wired `oracle_freshness` into `nightly.yml`'s oracle suite (unit/integration
  freshness tests already ran under plain `cargo test` in `ci.yml`, no PR-CI
  change was needed for that half).

**Explicitly deferred, tracking-only, NOT implemented (EH-0018):** a
centralized index dir (e.g. `~/.cache/syntext/<repo-id>/` instead of the
in-tree `.syntext/`, for multi-worktree sharing) and an `st watch` daemon
(proactive filesystem-notification-driven freshness instead of the current
reactive per-search bounded update). Both are scoped out of this board on
purpose -- do not implement them as part of a rerun of this board; they
belong to a future task list. `st watch` in particular should wait on the
on-disk generation persistence gap below being closed first, or it becomes a
second copy of the same cross-process staleness problem.

**Board invariants this run honored (keep honoring on rerun):**
- One card in `doing` at a time; move to `doing`, finish one bite, sync,
  repeat -- never batch multiple cards concurrently.
- A card only moves to `done` when every acceptance checkbox is checked; a
  card with unchecked acceptance items does not move to `done` even if all
  its bites are checked off (this is why the first pass correctly left
  EH-0009 in `doing` instead of force-completing it).
- No status was force-moved by either write-back pass -- board state reflects
  genuine run progress, not manual editing.

**Known gap, still open after this board (not blocked, just not in scope --
carry forward as a constraint for any board that touches freshness or
`st watch`):** the background `st update` catch-up child only updates its own
process's in-memory `ArcSwap<IndexSnapshot>`. `Manifest::overlay_gen`'s doc
comment (`src/index/manifest.rs`) still says on-disk generation files are
"not yet written (deferred to a later milestone)" -- confirmed still true
after this board (paths.idx persists `PathIndex` only, not overlay
generations). So a *separate, later* `st search` process does not see edits
committed by an async catch-up spawned by an earlier search's process. This
was flagged in EH-0003's original notes as a candidate to fold into EH-0009's
persistence work, but EH-0009 shipped scoped to `PathIndex` only -- the gap
was not closed and is not tracked by any other card on this board. Whoever
plans the next freshness-related board (or `st watch`, per EH-0018) should
open a card for it explicitly rather than assume paths.idx covered it.

**Note on card bodies:** the markdown files for EH-0001..EH-0013 in
`tasks/done/` currently have empty `## One-bite-at-a-time plan` / `## Notes`
bodies (0 bites listed, despite `activity.jsonl` showing bites 1-3 were
checked off during the run) -- an apparent sync/resume artifact from the
board being picked up across two passes, not evidence the work wasn't done.
The completion claims above for EH-0001..EH-0008 were cross-checked against
this same file's prior write-back (still readable in git history if this
section is ever replaced) and against real source (e.g. `src/cli/catchup.rs`,
`src/hook/vendors/githooks.rs` exist and are non-trivial); EH-0009..EH-0017's
claims above were cross-checked against the actual diffs/new files
(`paths_idx.rs`, `dict_read.rs`, `oracle_freshness.rs`, `benches/freshness.rs`,
`benches/open_search_e2e.rs`, `nightly.yml`, `docs/BENCHMARKS.md`) rather than
card notes, since only EH-0014..EH-0017 retained detailed `## Notes`.

**Manual verification residuals (2026-07-09):** one residual-style flag
survived only in the board's `state.json` acceptance history (not in any
current card's `## Notes`, so it would otherwise be lost): an earlier
acceptance-criteria draft for EH-0014 read "`st init --fsmonitor` sets
`core.fsmonitor=true` in a temp repo -- asserted via `git config
core.fsmonitor` in a subprocess test (**manual residual: interactive prompt
path**)". The shipped implementation is flag-only (`st init --fsmonitor`,
`cmd_init_fsmonitor` in `src/cli/mod.rs`) with no stdin-driven interactive
prompt found in the current code, so this may be stale -- but a human should
still spot-check, in a real terminal against a real git repo:
1. Run `st search` (or trigger a stale search) in a git repo with
   `core.fsmonitor` unset and confirm `maybe_print_fsmonitor_tip`'s one-time
   tip actually prints with the expected wording/timing (not just that the
   unit test around it passes).
2. Run `st init --fsmonitor` in that same repo and confirm
   `git config core.fsmonitor` is really set afterward, matching what the
   subprocess test asserts in CI.

Separately (not from a `residual:` tag, but the same "needs a live runtime to
observe" character): manually verify the cross-process staleness gap above by
running `st update` (or triggering an auto-update via search) in one process,
then running `st search --no-update <new-content>` in a *second*, fresh
process against the same index dir, and confirming the second process still
does not see the update -- this is the live behavior the "known gap" section
describes, and it is easy to accidentally fix half of (e.g. via a paths.idx-
adjacent change) without noticing the overlay-generation persistence itself
is still missing.
