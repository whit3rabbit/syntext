# Benchmarks

This document explains how to set up, execution, methodology, and the findings of performance benchmarks for `ripline`. 

## Setup & Execution

### macOS Corpus Setup

Shallow clones are enough for search benchmarks:

```sh
mkdir -p ./_ripline-bench
git clone --depth 1 -b v6.8 https://github.com/torvalds/linux.git ./_ripline-bench/linux
git clone --depth 1 -b 1.77.0 https://github.com/rust-lang/rust.git ./_ripline-bench/rust
git clone --depth 1 -b v18.2.0 https://github.com/facebook/react.git ./_ripline-bench/react
git clone --depth 1 -b v5.4.3 https://github.com/microsoft/TypeScript.git ./_ripline-bench/typescript
git clone --depth 1 -b v20.12.0 https://github.com/nodejs/node.git ./_ripline-bench/node
```

> **macOS warning**: default APFS is case-insensitive. `linux` and `rust` have case-colliding paths, so Git will warn and only one path from each collision set will exist in the working tree. That is acceptable for rough performance work, but not for strict corpus-correctness claims.

### Synthetic Corpus Benchmarks

Use these when changing tokenizer coverage, posting execution, query routing, or commit-path performance.

Run Criterion benches **sequentially**, one benchmark target at a time. Do not run multiple bench targets in parallel or use one shell command that launches several back-to-back:

```sh
cargo bench --bench query_latency -- --sample-size 10
cargo bench --bench index_build -- --sample-size 10
cargo bench --bench selectivity -- --sample-size 10
```

### External Repository Comparison Harness

Use the Python harness for `ripline` vs `rg` vs `grep` on a real repo. This tests end-to-end performance.

```sh
python3 scripts/bench_compare.py \
  --repo /path/to/repo \
  --query literal:token_aligned_symbol \
  --query literal:another_symbol
```

Use the preset catalog in `benchmarks/repo_presets.json`. We recommend starting with these presets:

1. `react_token_aligned`
2. `rust_token_aligned`
3. `linux_token_aligned`
4. `typescript_compiler`
5. `node_runtime`

#### Standard Usage

List presets:
```sh
python3 scripts/bench_compare.py --list-presets
```

Run a preset:
```sh
python3 scripts/bench_compare.py --preset react_token_aligned
```

Emit a copy-paste-friendly Markdown table only:

```sh
python3 scripts/bench_compare.py --preset react_token_aligned --markdown-table-only
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --markdown-table-only \
  --output /tmp/react-bench.md
```

Measure repeated queries against one already-opened `ripline` index in a single process:

```sh
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --ripline-search-mode persistent
```

Use `--ripline-search-mode persistent` when you want to measure query-time reuse, such as snapshot-scoped posting bitmap caches, without paying process startup and index open cost on every `ripline` search. Keep `fork` as the default for apples-to-apples CLI comparisons, since `rg` and `grep` still run one process per query in this harness.

Report both `ripline` modes side by side in one run:

```sh
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --ripline-search-mode both
```

Use `both` when you want one report that shows the gap between CLI-style cold searches (`ripline-fork`) and hot in-process searches (`ripline-persistent`).

For very large repos, or large-corpus preset:
```sh
python3 scripts/bench_compare.py \
  --preset linux_token_aligned \
  --build-iterations 1 \
  --search-iterations 1 \
  --warmups 0
```

## Testing Rules & Query Discipline

- Prefer token-aligned literal queries. Current `ripline` coverage guarantees are strongest there.
- Do not use substring-heavy literals like `ReactElement`, `useEffect`, or `TyCtxt` as headline benchmark terms unless exact count agreement has already been verified, since real code might embed substrings (e.g. `ReactElement` inside `ReactElementPropsTypeDestructor`).
- If `ripline`, `rg`, and `grep` counts differ, treat the timing comparison as suspect until the mismatch is explained.

## Calibrated Scan Threshold (2026-03-27)

Feature: replaced hard-coded 10% cardinality threshold in `should_use_index()` with a value computed at build time from actual I/O and posting-decode latency. Threshold is stored in `manifest.json` and loaded into `IndexSnapshot` on open.

**Build-time cost:** `calibrate_threshold()` reads up to 100 files and runs 20 Roaring bitmap AND iterations. On the synthetic test corpus this is immeasurable relative to total build time.

**Query routing impact:** Queries near the old 10% crossover may route differently. Broad literals (>50% cardinality) and very selective literals (<1% cardinality) are unaffected. The calibrated value for a warm NVMe + small files corpus is clamped to [0.01, 0.50]; typical values on NVMe are near 0.50 because in-memory bitmap AND cost is negligible compared to file read cost.

**Synthetic corpus query latency (post-feature, `--sample-size 10`):**

| Benchmark | Time (mean) | Range |
|---|---|---|
| `query_latency/literal_common` | 5.94 ms | 5.74–6.27 ms |
| `query_latency/indexed_regex_rare` | 153.9 µs | 152.2–156.3 µs |
| `query_latency/full_scan_regex` | 6.19 ms | 5.92–6.60 ms |

No regression vs. prior baseline (broad literal and full-scan regex are I/O dominated; selective indexed regex is index-dominated and unaffected by threshold change).

## Real World Results & Limitations

These points summarize current known testing observations against real code environments.

### Baseline Latencies
- **Query Latency**: Selective indexed regex is much cheaper than broad literal and full-scan regex on the synthetic corpus.
- **Commit Latency**: Single-file incremental commit is completely sub-millisecond, averaging `~135us` for single edits. 

### External Constraints
- **Selectivity**: On a broad common literal (`workspace`), `rg` is slightly faster than `ripline` since the candidate set is huge and verification dominates execution. On more selective literals, e.g. `LanguageServerId`, `ripline` operates 5x-10x faster (e.g. `8ms` vs `45ms`).
- Exact match preset counts are clean for queries like `useState`, `getDisplayNameForReactElement`, `rustc_middle`, and `mir::Body` on real codebases. Substring and suffix matches (such as `TyCtxt` or `useEffect`) will often undercount in `ripline` on big codebases and therefore serve as poor benchmark choices.
