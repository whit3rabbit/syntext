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
cargo bench --bench query_latency -- --sample-size 10
cargo bench --bench index_build -- --sample-size 10
cargo bench --bench selectivity -- --sample-size 10
```

## Benchmarking Reference

Run Criterion benches **sequentially**, one benchmark target at a time. Do not
run multiple bench targets in parallel, and do not use one shell command that
launches several Criterion benches back-to-back when collecting numbers for docs.

### Synthetic corpus benchmarks

```sh
cargo bench --bench query_latency -- --sample-size 10
cargo bench --bench index_build -- --sample-size 10
cargo bench --bench selectivity -- --sample-size 10
```

Use these when changing tokenizer coverage, posting execution, query routing,
or commit-path performance. Record before/after results in
`docs/BENCHMARKS.md`.

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

## Project Structure

```
src/
  lib.rs                      # public API (Index, Config, SearchOptions)
  main.rs                     # binary entry point (st); empty stub on wasm32
  wasm.rs                     # wasm-bindgen WasmIndex public API (wasm feature only)
  base64.rs                   # base64 encoding helpers
  path_util.rs                # path normalization utilities
  tokenizer/
    mod.rs                    # sparse n-gram extraction (build_all, forced boundaries)
    covering.rs               # build_covering and build_covering_inner (query gram extraction)
    weights.rs                # pre-trained [u16; 65536] byte-pair frequency table
    tests.rs                  # unit tests for tokenizer
  index/
    mod.rs                    # Index struct, top-level re-exports
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
