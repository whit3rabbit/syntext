# ripline-rs

Hybrid code search index for agent workflows. Sparse n-gram content index + Roaring bitmap path index + optional Tree-sitter symbol index.

> **Crate name:** `ripline-rs` (the name `ripline` was already taken on crates.io). Binary is still `ripline`.

- See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for quantitative design reasoning.

## Architecture

- **Segment format**: immutable single-file segments (RPLX magic, TOC footer, page-aligned dictionary). Postings are delta-varint or Roaring bitmap depending on list size (threshold: 8K entries).
- **Tokenizer**: two-tier boundary detection (forced boundaries at code delimiters + weight-based within alphanumeric spans). Lowercase normalization at index/query time. Weight table lives in `src/tokenizer/weights.rs`. Forced boundary set in `is_forced_boundary()` in `src/tokenizer/mod.rs`.
- **Query router**: literal (`build_covering` + memchr::memmem) / indexed regex (`build_covering_inner` + HIR decomposition) / full scan. Cardinality-based fallback skips index when smallest posting list > 10% of total docs. Path filter always first.
- **Overlay**: single merged in-memory OverlayView, rebuilt from all dirty files on each `commit_batch()`. ArcSwap<IndexSnapshot> for snapshot isolation. On-disk generations for crash recovery only.
- **Build**: batched-segment construction, 256MB per batch, sort-based aggregation, rayon for parallelism. Peak memory ~1.5GB per batch.

## General Rules

- **Never put full file paths in documents.** Always use relative paths when referencing internal files. When referencing external repositories, use links to the original Git repository and lock it to a specific version or tag when possible.

## Dependencies

| Crate | Purpose |
|---|---|
| regex, regex-syntax | verification engine + HIR walking for gram decomposition |
| memchr | literal verification fast path (memmem) |
| memmap2 | mmap segment files |
| roaring | bitmap posting lists (dense terms), path index component sets |
| rayon | parallel index build |
| zerocopy | zero-copy segment reads paired with memmap2 |
| arc-swap | lock-free snapshot swapping for concurrent reads |
| ignore | .gitignore respect, file-type filtering |
| clap | CLI (with derive) |
| serde, serde_json | manifest serialization |
| uuid | segment IDs (v4) |
| xxhash-rust | checksums (xxh64) |
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

## Commands

```
cargo test                    # unit + integration tests
cargo clippy                  # lint, must pass with no warnings
cargo bench                   # criterion benchmarks
RIPLINE_LOG_SELECTIVITY=1 cargo test --test correctness -- --nocapture
                              # show per-query selectivity stats
cargo test --test tokenizer   # unit: tokenizer only
cargo test --test posting     # unit: posting lists only
cargo test --test query       # unit: query router only
cargo test --test overlay     # unit: overlay/snapshot only
cargo test --test boundary_fuzz  # unit: boundary fuzzing
cargo test --test index_build # integration: index construction
cargo test --test incremental # integration: incremental updates
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

Use the Python harness for `ripline` vs `rg` vs `grep` on a real repo:

```sh
python3 scripts/bench_compare.py \
  --repo /path/to/repo \
  --query literal:token_aligned_symbol \
  --query literal:another_symbol
```

Useful options:

```sh
python3 scripts/bench_compare.py --help
python3 scripts/bench_compare.py --list-presets
python3 scripts/bench_compare.py --json --repo /path/to/repo --query literal:foo
python3 scripts/bench_compare.py --preset react_token_aligned --markdown-table-only
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --markdown-table-only \
  --output /tmp/react-bench.md
python3 scripts/bench_compare.py \
  --preset react_token_aligned
python3 scripts/bench_compare.py \
  --build-iterations 1 \
  --search-iterations 1 \
  --warmups 0 \
  --preset rust_token_aligned
python3 scripts/bench_compare.py \
  --preset zed_mixed_app \
  --ripline-search-mode both
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --build-only \
  --build-iterations 5 \
  --json
```

For very large repos, start with `--build-iterations 1 --search-iterations 1 --warmups 0`
and only increase iterations if the runtime is acceptable.

Use `--ripline-search-mode both` when you want one report that shows both
CLI-style cold searches and hot in-process searches against one already-opened
index. Use `--build-only` when tokenizer or index-build changes are the thing
you are measuring, because it records repeated build latency and full on-disk
index-directory byte totals without mixing in search timings.

Use the preset catalog in [`benchmarks/repo_presets.json`](benchmarks/repo_presets.json)
and the rationale in [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).
The benchmark process should stay on this one script and this one preset catalog
so runs are reproducible over time.

### macOS corpus setup

Shallow clones are enough for search benchmarks:

```sh
mkdir -p ./_ripline-bench
git clone --depth 1 -b v6.8 https://github.com/torvalds/linux.git ./_ripline-bench/linux
git clone --depth 1 -b 1.77.0 https://github.com/rust-lang/rust.git ./_ripline-bench/rust
git clone --depth 1 -b v18.2.0 https://github.com/facebook/react.git ./_ripline-bench/react
git clone --depth 1 -b v5.4.3 https://github.com/microsoft/TypeScript.git ./_ripline-bench/typescript
git clone --depth 1 -b v20.12.0 https://github.com/nodejs/node.git ./_ripline-bench/node
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

- Prefer token-aligned literal queries. Current `ripline` coverage guarantees are
  strongest there.
- Do not use substring-heavy literals like `ReactElement`, `useEffect`, or
  `TyCtxt` as headline benchmark terms unless exact count agreement has already
  been verified.
- If `ripline`, `rg`, and `grep` counts differ, treat the timing comparison as
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
- `'qz'`, `'xj'`: weight > 35000 (rare, good gram boundaries)
- Unseen pairs: weight 65535
- Non-zero pairs: ~15000+

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
  main.rs                     # binary entry point
  tokenizer/
    mod.rs                    # sparse n-gram extraction (build_all, build_covering)
    weights.rs                # pre-trained [u16; 65536] byte-pair frequency table
  index/
    mod.rs                    # index builder + reader
    segment.rs                # RPLX segment format (read/write)
    overlay.rs                # OverlayView + ArcSwap<IndexSnapshot>
    manifest.rs               # manifest.json + atomic write-then-rename
    merge.rs                  # background segment merge
    walk.rs                   # directory walking / file discovery
  posting/
    mod.rs                    # posting list types + adaptive intersection/union
    roaring_util.rs           # Roaring bitmap integration for dense terms
  query/
    mod.rs                    # query router (literal / indexed regex / full scan)
    regex_decompose.rs        # HIR walker -> GramQuery tree
    planner.rs                # cardinality-based intersection ordering
  search/
    mod.rs                    # search executor
    verifier.rs               # tiered: memchr for literals, regex for patterns
  path/
    mod.rs                    # Roaring bitmap component index
    filter.rs                 # glob/type scope filters
  symbol/
    mod.rs                    # Tree-sitter + SQLite symbol index
    extractor.rs              # symbol extraction from parse trees
  cli/
    mod.rs                    # clap CLI entry point

specs/001-hybrid-code-search-index/
  spec.md                     # feature specification
  plan.md                     # implementation plan
  research.md                 # architecture research (19 sections)
  data-model.md               # entity definitions
  contracts/                  # library API, CLI, segment format contracts
  quickstart.md               # usage guide
  tasks.md                    # implementation task breakdown
```

## Spec Location

All design documents are in `specs/001-hybrid-code-search-index/`. When in doubt about a design decision, check `research.md` first -- it has 19 sections covering every major subsystem with Decision / Rationale / Alternatives Considered.
