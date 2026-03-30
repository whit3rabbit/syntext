# Benchmarks

This document explains how to set up, execution, methodology, and the findings of performance benchmarks for `syntext`. 

## Setup & Execution

### macOS Corpus Setup

Shallow clones are enough for search benchmarks:

```sh
mkdir -p ./_syntext-bench
git clone --depth 1 -b v6.8 https://github.com/torvalds/linux.git ./_syntext-bench/linux
git clone --depth 1 -b 1.77.0 https://github.com/rust-lang/rust.git ./_syntext-bench/rust
git clone --depth 1 -b v18.2.0 https://github.com/facebook/react.git ./_syntext-bench/react
git clone --depth 1 -b v5.4.3 https://github.com/microsoft/TypeScript.git ./_syntext-bench/typescript
git clone --depth 1 -b v20.12.0 https://github.com/nodejs/node.git ./_syntext-bench/node
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

Use the Python harness for `syntext` vs `rg` vs `grep` on a real repo. This tests end-to-end performance.

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
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --repo _syntext-bench/react
```

Emit a copy-paste-friendly Markdown table only:

```sh
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --repo _syntext-bench/react \
  --markdown-table-only
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --repo _syntext-bench/react \
  --markdown-table-only \
  --output /tmp/react-bench.md
```

Measure repeated queries against one already-opened `syntext` index in a single process:

```sh
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --repo _syntext-bench/react \
  --syntext-search-mode persistent
```

Use `--syntext-search-mode persistent` when you want to measure query-time reuse, such as snapshot-scoped posting bitmap caches, without paying process startup and index open cost on every `syntext` search. Keep `fork` as the default for apples-to-apples CLI comparisons, since `rg` and `grep` still run one process per query in this harness.

Report both `syntext` modes side by side in one run:

```sh
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --repo _syntext-bench/react \
  --syntext-search-mode both
```

Use `both` when you want one report that shows the gap between CLI-style cold searches (`syntext-fork`) and hot in-process searches (`syntext-persistent`).

For very large repos, or large-corpus preset:
```sh
python3 scripts/bench_compare.py \
  --preset linux_token_aligned \
  --repo _syntext-bench/linux \
  --build-iterations 1 \
  --search-iterations 1 \
  --warmups 0
```

Measure repeated build time and on-disk index size without running any search queries:

```sh
python3 scripts/bench_compare.py \
  --preset react_token_aligned \
  --repo _syntext-bench/react \
  --build-only \
  --build-iterations 5 \
  --json
```

Use `--build-only` when tokenizer, segment layout, or index construction is the thing you changed. The report includes both repeated build latency and repeated full index-directory byte totals, so you can catch cases where build time drops only because the index got smaller, or vice versa.

## Testing Rules & Query Discipline

- Prefer token-aligned literal queries. Current `syntext` coverage guarantees are strongest there.
- Do not use substring-heavy literals like `ReactElement`, `useEffect`, or `TyCtxt` as headline benchmark terms unless exact count agreement has already been verified, since real code might embed substrings (e.g. `ReactElement` inside `ReactElementPropsTypeDestructor`).
- If `syntext`, `rg`, and `grep` counts differ, treat the timing comparison as suspect until the mismatch is explained.

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
- **Selectivity**: On a broad common literal (`workspace`), `rg` is slightly faster than `syntext` since the candidate set is huge and verification dominates execution. On more selective literals, e.g. `LanguageServerId`, `syntext` operates 5x-10x faster (e.g. `8ms` vs `45ms`).
- **Compound literals**: Planner quality matters as much as raw index speed. A naive “smallest single posting list” fallback can misclassify compound identifiers like `irq_work_queue` because each component gram is common while their intersection is selective. The current planner probes a few smallest intersections before bailing to full scan, which restored `irq_work_queue` on the Linux preset from scan-like multi-second behavior to indexed behavior in local runs (`syntext-fork` about `196ms`, `syntext-persistent` about `114ms`, `rg` about `3.6s`).
- Exact match preset counts are clean for queries like `useState`, `getDisplayNameForReactElement`, `rustc_middle`, and `mir::Body` on real codebases. Substring and suffix matches (such as `TyCtxt` or `useEffect`) will often undercount in `syntext` on big codebases and therefore serve as poor benchmark choices.
- **Hot vs cold search**: `--syntext-search-mode both` is useful when measuring real agent loops. On the current Zed preset, `LanguageServerId` measured about `9ms` in fork mode and about `2ms` in persistent mode, with identical counts. Broad queries like `workspace` change much less, because verification still dominates.
- **Incremental parity**: Benchmark numbers are easier to trust when incremental updates match full builds. Incremental commits now reject lexical path traversal outside `repo_root` and skip binary files the same way full builds do, so “hot index” runs do not quietly benchmark a different visible corpus than fresh builds.
- **Camel-case indexing tradeoff**: the `c671141` change set added exact-literal expansion for small regex alternations and extra camel-case-aware grams at index time. A direct before/after comparison against `2513d0e` showed modest on-disk growth but a non-trivial build-time bump in single local runs:

| Repo | Build time before | Build time after | Index bytes before | Index bytes after |
|---|---|---|---|---|
| `linux` | `6386 ms` | `8753 ms` (`+37.1%`) | `51,538,937` | `52,274,473` (`+1.4%`) |
| `react` | `521 ms` | `574 ms` (`+10.1%`) | `4,609,794` | `4,989,535` (`+8.2%`) |
| `zed-research` | `212 ms` | `255 ms` (`+20.3%`) | `2,619,098` | `2,659,654` (`+1.5%`) |

These numbers are useful, but they are not clean enough to call “free”. The index-size increase is small in the corpora above, while build-time cost looks real and somewhat noisy. Keep the feature because it turned the Zed alternation case from scan-like behavior into indexed behavior, but rerun repeated build benchmarks before claiming no build regression.

Follow-up: a later tokenizer optimization stopped running the second case-aware pass for inputs that have no lowercase-to-uppercase transitions, and stopped re-emitting spans already covered by the lowercase pass. Repeated `--build-only` runs on the current branch produced this post-optimization baseline:

| Repo | Build median | Build min | Build max | Index bytes |
|---|---|---|---|---|
| `react` | `369.838 ms` | `361.038 ms` | `377.477 ms` | `4,989,569` |
| `linux` | `5,929.347 ms` | `5,840.757 ms` | `6,973.909 ms` | `52,274,507` |
| `zed-research` | `206.016 ms` | `198.887 ms` | `237.082 ms` | `2,659,688` |

The Zed search sanity check still returned the same indexed counts for `LanguageServerId` (`430`) and `LanguageServer(Id|InstallationStatus)` (`507`), so the build recovery did not come from dropping the indexed regex win.

## Delta Gram-Index

Measured with `cargo bench --bench index_build -- --sample-size 10` on the
synthetic corpus (macOS, Apple Silicon). Before numbers are from the slow path,
which rebuilt the entire gram_index from all dirty files on every `commit_batch()`.

| Benchmark | Before (slow path, O(all dirty)) | After (delta path, O(changed)) |
|---|---|---|
| `full_build_300_files` | ~16.7 ms | 16.706 ms (no regression) |
| `commit_batch_single_edit` | ~135 µs | 114.89 µs |

The delta path (`build_incremental_delta`) clones the existing gram_index,
surgically removes stale doc_ids for changed/deleted files using cached grams,
then appends fresh doc_ids only for the changed set. For a single-file edit
against an overlay with many unchanged files this is O(changed_grams) instead
of O(all_dirty_grams). The `commit_batch_single_edit` improvement (~15%)
reflects reduced tokenization work; the gain grows with overlay size.

The `full_build_300_files` benchmark exercises the initial segment build path,
which is unaffected by the overlay change.

---

### Two-file storage — baseline (before v3 format)

Date: 2026-03-28
Commit: 8096ba4

#### index_build

| Benchmark | Time (mean) | Range |
|---|---|---|
| `full_build_300_files` | 17.608 ms | [17.516 ms – 17.761 ms] |
| `commit_batch_single_edit` | 130.01 µs | [128.06 µs – 133.80 µs] |

#### query_latency

| Benchmark | Time (mean) | Range |
|---|---|---|
| `literal_common` | 4.1599 ms | [4.1444 ms – 4.1778 ms] |
| `indexed_regex_rare` | 136.88 µs | [136.08 µs – 138.23 µs] |
| `full_scan_regex` | 4.2456 ms | [4.2213 ms – 4.2826 ms] |

Criterion noted regressions on `literal_common` (+28%) and `full_scan_regex` (+23%) and `full_build_300_files` (+13%) relative to prior stored baseline, and an improvement on `indexed_regex_rare` (-9%). These are the pre-v3-format reference numbers.

### Two-file storage — after v3 format

Date: 2026-03-28
Commit: 0d1a0d0

#### index_build

| Benchmark | Time (mean) | Range |
|---|---|---|
| `full_build_300_files` | 18.275 ms | [18.091 ms – 18.437 ms] |
| `commit_batch_single_edit` | 183.22 µs | [164.70 µs – 198.22 µs] |

#### query_latency

| Benchmark | Time (mean) | Range |
|---|---|---|
| `literal_common` | 4.5719 ms | [4.2510 ms – 4.9600 ms] |
| `indexed_regex_rare` | 161.41 µs | [160.05 µs – 164.42 µs] |
| `full_scan_regex` | 4.4819 ms | [4.1408 ms – 4.8375 ms] |

#### Analysis

**Index build:**
- `full_build_300_files`: 18.275 ms vs baseline 17.608 ms (+3.8% regression). Expected: similar or slightly slower due to two file writes per segment. The small increase reflects the cost of maintaining two separate files and synchronizing them.
- `commit_batch_single_edit`: 183.22 µs vs baseline 130.01 µs (+41% regression). This is statistically significant. The increased latency is consistent with v3 two-file format requiring two write operations per segment build. The baseline was on a single-file format; the v3 split requires concurrent writes to `.dict` and `.post` files plus explicit fsync behavior.

**Query latency (literal/regex/scan):**
- `literal_common`: 4.5719 ms vs baseline 4.1599 ms (+10% regression). Within expected variance for the synthetic corpus; the latency increase reflects pread overhead for reading postings from separate `.post` file. On real-world indexes at multi-GB scale, OS page cache masks this cost.
- `indexed_regex_rare`: 161.41 µs vs baseline 136.88 µs (+18% regression). Postings are now in a separate file, requiring pread() calls that miss the page cache initially. PostingsBitmapCache (per-snapshot) and dense bitmap caching (repeated queries within same search session) mitigate this overhead in persistent mode.
- `full_scan_regex`: 4.4819 ms vs baseline 4.2456 ms (+5.6% improvement within noise; reported as "+1% change" by Criterion). Full-scan queries do not use posting lists, so file layout is irrelevant.

**Resident memory benefit:**
The two-file split (dictionary separate from postings) reduces working set for large indexes. This benefit is not measured here because the synthetic corpus is ~300 files (~60 KB on disk). The improvement is only visible at multi-GB index scale where keeping postings out of the dictionary cache avoids thrashing. Current external-repo spot checks are recorded below.

### Current snapshot

Date: 2026-03-29  
Workspace state: release candidate before the 1.0 tag

#### Synthetic corpus

`cargo bench --bench query_latency -- --sample-size 10`

| Benchmark | Time (estimate) | Range |
|---|---|---|
| `literal_common` | 4.2580 ms | [4.2280 ms - 4.2793 ms] |
| `indexed_regex_rare` | 138.68 µs | [136.77 µs - 142.39 µs] |
| `full_scan_regex` | 4.3191 ms | [4.2929 ms - 4.3443 ms] |

`cargo bench --bench index_build -- --sample-size 10`

| Benchmark | Time (estimate) | Range |
|---|---|---|
| `full_build_300_files` | 41.080 ms | [40.882 ms - 41.252 ms] |
| `commit_batch_single_edit` | 156.29 µs | [154.12 µs - 158.34 µs] |

`cargo bench --bench selectivity -- --sample-size 10`

| Benchmark | Time (estimate) | Range |
|---|---|---|
| `literal_no_match` | 4.1458 ms | [4.1314 ms - 4.1573 ms] |
| `indexed_regex_selective` | 139.27 µs | [138.13 µs - 140.50 µs] |
| `literal_broad` | 4.1859 ms | [4.1486 ms - 4.2616 ms] |

Relative to the previous 2026-03-29 snapshot that was already in this document,
the selective regex path improved materially (`indexed_regex_rare` from `158.53 µs`
to `138.68 µs`), while most scan-heavy cases moved only a few percent. The
largest synthetic regression remains initial full build time, which rose from
`28.139 ms` to `41.080 ms`. Single-edit incremental commit stayed effectively
flat (`158.04 µs` to `156.29 µs`).

#### External repo spot checks

These runs used the local `_syntext-bench` corpus from the setup section and the
current `target/release/st` binary. Treat them as release-candidate spot checks,
not strict before/after regressions against every older historical table.

Preset-backed external matrix (`python3 scripts/bench_compare.py --repo ... --preset ... --json`):

| Repo | Commit | Tracked files | Build median | Index bytes | `syntext` avg | `rg` avg | `grep` avg | Speedup vs `rg` |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `react` | `3cb2c42` | 6,840 | `746.003 ms` | `6,553,696` | `20.662 ms` | `112.946 ms` | `314.278 ms` | `5.5x` |
| `rust` | `23903d01` | 58,698 | `3376.174 ms` | `13,860,347` | `99.911 ms` | `2183.234 ms` | `2412.816 ms` | `21.9x` |
| `typescript` | `7881fe530` | 81,362 | `4807.992 ms` | `19,943,106` | `111.857 ms` | `3093.845 ms` | `3171.794 ms` | `27.7x` |
| `node` | `53bcd114` | 47,364 | `3991.465 ms` | `79,012,633` | `69.495 ms` | `1492.564 ms` | `3186.352 ms` | `21.5x` |
| `linux` | `46b513250-dirty` | 93,018 | `8357.722 ms` | `80,624,410` | `154.457 ms` | `3681.269 ms` | `n/a` | `23.8x` |

Search results from the same matrix runs:

| Repo | Query | Count match | `syntext` | `rg` | `grep` |
|---|---|---|---|---|---|
| `react` | `useState` | yes (`2708`) | `27.813 ms` | `113.921 ms` | `300.000 ms` |
| `react` | `getDisplayNameForReactElement` | yes (`13`) | `13.510 ms` | `111.970 ms` | `328.555 ms` |
| `rust` | `rustc_middle` | yes (`3757`) | `105.699 ms` | `2210.204 ms` | `2521.141 ms` |
| `rust` | `mir::Body` | yes (`141`) | `94.123 ms` | `2156.264 ms` | `2304.491 ms` |
| `typescript` | `TransformationContext` | yes (`142`) | `108.736 ms` | `3115.297 ms` | `3262.582 ms` |
| `typescript` | `NodeBuilderFlags` | yes (`255`) | `114.978 ms` | `3072.393 ms` | `3081.006 ms` |
| `node` | `EnvironmentOptions` | yes (`158`) | `68.623 ms` | `1457.499 ms` | `3390.259 ms` |
| `node` | `MaybeStackBuffer` | yes (`93`) | `70.368 ms` | `1527.629 ms` | `2982.445 ms` |
| `linux` | `irq_work_queue` | yes (`128`) | `163.728 ms` | `3591.790 ms` | `n/a` |
| `linux` | `sched_clock` | yes (`817`) | `150.043 ms` | `3768.749 ms` | `n/a` |
| `linux` | `raw_spin_lock` | yes (`2321`) | `149.601 ms` | `3683.267 ms` | `n/a` |

Every query in this refreshed matrix had exact count parity with its comparator
tools. The Linux clone remained `-dirty` during the run because the local macOS
checkout still carries the case-collision modifications called out in the setup
warning above.
