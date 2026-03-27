# External Repository Benchmarks

These measurements complement the synthetic Criterion benches in
[`docs/PERFORMANCE_BASELINE.md`](/Users/whit3rabbit/Documents/GitHub/ripline/docs/PERFORMANCE_BASELINE.md).
They answer a different question: how does `ripline` compare to `rg` and
`grep` on a real multi-thousand-file codebase?

## Harness

Use [`scripts/bench_compare.py`](/Users/whit3rabbit/Documents/GitHub/ripline/scripts/bench_compare.py).

Example:

```sh
python3 scripts/bench_compare.py \
  --repo /path/to/linux \
  --query literal:schedule \
  --query literal:spin_lock \
  --query 'regex:spin_(lock|unlock)'
```

What the script does:

- Measures `ripline index` separately.
- Reuses one built index for repeated `ripline search` timings.
- Benchmarks `rg` on the same repo with `--hidden`.
- Benchmarks `grep` over `git ls-files` by default, which is a fairer baseline
  than raw recursive grep through ignored/build output.

## Current run

Date: `2026-03-26`

Repo: `/Users/whit3rabbit/Documents/GitHub/zed-research`

- Tracked files: `3593`
- Grep mode: `tracked`
- Ripline build iterations: `3`
- Search iterations per tool/query: `5`

### Ripline index build

- median: `225.103 ms`
- min: `217.698 ms`
- max: `226.656 ms`

### Search latency

| Query | Tool | Matches | Median ms | Min ms | Max ms |
|---|---:|---:|---:|---:|---:|
| `literal:workspace` | `ripline` | `14727` | `50.39` | `50.013` | `51.191` |
| `literal:workspace` | `rg` | `14727` | `45.47` | `43.038` | `48.123` |
| `literal:workspace` | `grep` | `14727` | `233.07` | `225.623` | `235.538` |
| `literal:LanguageServerId` | `ripline` | `430` | `8.391` | `8.261` | `8.96` |
| `literal:LanguageServerId` | `rg` | `430` | `44.718` | `44.346` | `45.075` |
| `literal:LanguageServerId` | `grep` | `430` | `209.067` | `208.773` | `212.328` |
| `regex:LanguageServer(Id|InstallationStatus)` | `ripline` | `507` | `44.046` | `42.429` | `47.212` |
| `regex:LanguageServer(Id|InstallationStatus)` | `rg` | `507` | `45.159` | `43.035` | `46.012` |
| `regex:LanguageServer(Id|InstallationStatus)` | `grep` | `507` | `234.345` | `227.416` | `236.635` |

## Interpretation

- On a broad common literal (`workspace`), `rg` is still slightly faster than
  `ripline`. That is not surprising, because the candidate set is huge and
  verification dominates.
- On a more selective literal (`LanguageServerId`), `ripline` is materially
  faster than both `rg` and `grep`.
- On the tested regex, `ripline` and `rg` are roughly tied. That fits the
  current design: indexed regex narrowing exists, but regex still pays more
  verifier cost than selective literals.
- `grep` is much slower in every measured case, even with a tracked-file list.

## Cross-language token-aligned pass

Date: `2026-03-27`

Goal: rerun the external harness with cleaner literal terms across multiple
language families.

Settings:

- `--build-iterations 1`
- `--search-iterations 1`
- `--warmups 0`

### React

Repo: `/Users/whit3rabbit/Documents/GitHub/_ripline-bench/react`

- Tracked files: `6840`
- `ripline index` build: `347.956 ms`

| Query | Tool | Matches | Median ms |
|---|---:|---:|---:|
| `literal:useState` | `ripline` | `2708` | `111.716` |
| `literal:useState` | `rg` | `2708` | `114.463` |
| `literal:useState` | `grep` | `2708` | `273.504` |
| `literal:useEffect` | `ripline` | `2564` | `21.232` |
| `literal:useEffect` | `rg` | `2578` | `191.945` |
| `literal:useEffect` | `grep` | `2578` | `310.818` |
| `literal:getDisplayNameForReactElement` | `ripline` | `13` | `10.354` |
| `literal:getDisplayNameForReactElement` | `rg` | `13` | `110.312` |
| `literal:getDisplayNameForReactElement` | `grep` | `13` | `315.521` |

### Rust

Repo: `/Users/whit3rabbit/Documents/GitHub/_ripline-bench/rust`

- Tracked files: `58698`
- `ripline index` build: `2755.613 ms`

| Query | Tool | Matches | Median ms |
|---|---:|---:|---:|
| `literal:rustc_middle` | `ripline` | `3757` | `107.084` |
| `literal:rustc_middle` | `rg` | `3757` | `1946.141` |
| `literal:rustc_middle` | `grep` | `3757` | `2307.596` |
| `literal:TyCtxt` | `ripline` | `4941` | `79.917` |
| `literal:TyCtxt` | `rg` | `5027` | `1900.225` |
| `literal:TyCtxt` | `grep` | `5027` | `1742.422` |
| `literal:mir::Body` | `ripline` | `141` | `77.344` |
| `literal:mir::Body` | `rg` | `141` | `1899.967` |
| `literal:mir::Body` | `grep` | `141` | `2049.872` |

### Linux

Repo: `/Users/whit3rabbit/Documents/GitHub/_ripline-bench/linux`

- Tracked files: `93018`
- Tools: `ripline`, `rg`
- `ripline index` build: `6101.93 ms`

| Query | Tool | Matches | Median ms |
|---|---:|---:|---:|
| `literal:irq_work_queue` | `ripline` | `128` | `3220.995` |
| `literal:irq_work_queue` | `rg` | `128` | `3432.32` |
| `literal:sched_clock` | `ripline` | `817` | `125.831` |
| `literal:sched_clock` | `rg` | `817` | `3771.862` |
| `literal:raw_spin_lock` | `ripline` | `2321` | `121.53` |
| `literal:raw_spin_lock` | `rg` | `2321` | `3820.238` |

### Takeaways

- `useState`, `getDisplayNameForReactElement`, `rustc_middle`, and `mir::Body`
  are clean comparison terms on these corpora. Counts matched exactly.
- `useEffect` and `TyCtxt` still were not clean literal probes. Both undercounted
  in `ripline`, which means they are still hitting mid-token substring cases in
  real code.
- The Linux preset now completes by using the same shared script with a cheaper
  preset tool set: `ripline` plus `rg`, without `grep`.

## Preset-backed TypeScript and Node runs

Date: `2026-03-27`

These runs use the shared preset catalog in
[`benchmarks/repo_presets.json`](/Users/whit3rabbit/Documents/GitHub/ripline/benchmarks/repo_presets.json)
through the same harness:

```sh
python3 scripts/bench_compare.py --preset typescript_compiler
python3 scripts/bench_compare.py --preset node_runtime
```

### TypeScript

Repo: `/Users/whit3rabbit/Documents/GitHub/_ripline-bench/typescript`

- Tracked files: `81362`
- `ripline index` build: `3659.839 ms`

| Query | Tool | Matches | Median ms |
|---|---:|---:|---:|
| `literal:TransformationContext` | `ripline` | `142` | `95.611` |
| `literal:TransformationContext` | `rg` | `142` | `2573.266` |
| `literal:TransformationContext` | `grep` | `142` | `3339.558` |
| `literal:NodeBuilderFlags` | `ripline` | `255` | `88.656` |
| `literal:NodeBuilderFlags` | `rg` | `255` | `2447.316` |
| `literal:NodeBuilderFlags` | `grep` | `255` | `3270.151` |

### Node.js

Repo: `/Users/whit3rabbit/Documents/GitHub/_ripline-bench/node`

- Tracked files: `47364`
- `ripline index` build: `2810.84 ms`

| Query | Tool | Matches | Median ms |
|---|---:|---:|---:|
| `literal:EnvironmentOptions` | `ripline` | `158` | `55.626` |
| `literal:EnvironmentOptions` | `rg` | `158` | `1119.841` |
| `literal:EnvironmentOptions` | `grep` | `158` | `3550.489` |
| `literal:MaybeStackBuffer` | `ripline` | `93` | `55.454` |
| `literal:MaybeStackBuffer` | `rg` | `93` | `1089.497` |
| `literal:MaybeStackBuffer` | `grep` | `93` | `3097.734` |

### Takeaways

- The shared preset workflow is now validated on React, Rust, TypeScript, Node.js,
  and Linux.
- The TypeScript and Node presets now use only exact-count default terms. The
  earlier `createProgram`, `SyntaxKind`, `Environment`, and `AsyncWrap`
  candidates were replaced because they undercounted in `ripline`.

## Caveats

- This document now mixes two kinds of runs: a completed `zed-research` pass
  and a later partial cross-language pass that included local `react`, `rust`,
  and a non-completing `linux` attempt. Do not compare rows across sections
  without checking the corpus and settings first.
- `ripline` search latency excludes index build time. Index build is reported
  separately because `rg` and `grep` are no-index tools.
- Literal benchmark queries should be token-aligned when possible. A query like
  `ReactElement` in the React repo is a bad comparison term because many hits
  are mid-token substrings inside larger identifiers such as
  `getDisplayNameForReactElement`, `ReactElementValidator`, or
  `ReactElementPropsTypeDestructor`. Current `ripline` coverage guarantees are
  strongest for token-aligned queries, so those substring-heavy literals can
  undercount versus `rg`.
- If we later benchmark a kernel checkout, use the same script and record the
  command and corpus details here instead of mixing results across different
  repos.
