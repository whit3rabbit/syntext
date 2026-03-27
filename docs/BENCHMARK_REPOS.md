# Recommended Benchmark Repositories

Use the same harness for every external benchmark run:

[`scripts/bench_compare.py`](/Users/whit3rabbit/Documents/GitHub/ripline/scripts/bench_compare.py)

Use the same preset catalog for repo/query selection:

[`benchmarks/repo_presets.json`](/Users/whit3rabbit/Documents/GitHub/ripline/benchmarks/repo_presets.json)

Do not invent ad hoc repo/query combinations in the middle of a perf task unless
you are debugging a specific anomaly. Use a preset first, then document any
override explicitly.

## Repo set

| Preset | Repo | Why it matters |
|---|---|---|
| `zed_mixed_app` | Zed Research | Mixed Rust and TypeScript application code. Good general-purpose “real app” benchmark. |
| `react_token_aligned` | React | Medium-sized JS/TS UI-heavy repo. Good frontend-focused corpus. |
| `rust_token_aligned` | Rust compiler | Large Rust-heavy compiler corpus. Good for token-aligned Rust identifiers at scale. |
| `linux_token_aligned` | Linux kernel | Kernel-scale C corpus. Best stress test for indexing and very large search space. |
| `typescript_compiler` | TypeScript | Large TS-only compiler repo. Cleaner TS benchmark than a mixed app tree. |
| `node_runtime` | Node.js | Mixed C++ and JavaScript runtime code. Good non-kernel systems benchmark. |

## Recommendation order

If we only have time for a few repos, use this order:

1. `react_token_aligned`
2. `rust_token_aligned`
3. `linux_token_aligned`
4. `typescript_compiler`
5. `node_runtime`

Reasoning:

- `react_token_aligned` is fast enough to rerun often and catches frontend-oriented behavior.
- `rust_token_aligned` gives a much larger real Rust corpus than this repo itself.
- `linux_token_aligned` is the stress test, but it is also the most expensive and the least forgiving on macOS.
- `typescript_compiler` and `node_runtime` add cleaner TS and C++/JS comparisons when we want broader language coverage.

Current validated exact-count preset terms:

- `react_token_aligned`: `useState`, `getDisplayNameForReactElement`
- `rust_token_aligned`: `rustc_middle`, `mir::Body`
- `typescript_compiler`: `TransformationContext`, `NodeBuilderFlags`
- `node_runtime`: `EnvironmentOptions`, `MaybeStackBuffer`

Current cheaper large-corpus preset:

- `linux_token_aligned`: useful kernel corpus. The preset now uses the same
  shared harness with `ripline` plus `rg`, skipping `grep` by default so the run
  finishes in a reasonable window on this machine.

## Standard usage

List presets:

```sh
python3 scripts/bench_compare.py --list-presets
```

Run a preset using its default local path and default query set:

```sh
python3 scripts/bench_compare.py --preset react_token_aligned
```

Run a preset but override the repo path:

```sh
python3 scripts/bench_compare.py \
  --preset rust_token_aligned \
  --repo /path/to/rust
```

Run a preset in cheap large-corpus mode:

```sh
python3 scripts/bench_compare.py \
  --preset linux_token_aligned \
  --build-iterations 1 \
  --search-iterations 1 \
  --warmups 0
```

## macOS notes

- Use shallow clones for benchmark corpora.
- APFS is usually case-insensitive, so `linux` and `rust` will report
  case-colliding paths during clone. That is acceptable for rough performance
  work, but not for strict search-correctness claims.
- Kernel-scale runs can still be too expensive under the current full
  `ripline` vs `rg` vs `grep` harness. The shared Linux preset avoids that by
  defaulting to `ripline` plus `rg`.

## Query discipline

- Prefer token-aligned literal identifiers.
- Avoid substring-heavy literals like `ReactElement`, `useEffect`, or `TyCtxt`
  as headline benchmark terms unless you have already confirmed exact count
  agreement across tools.
- If counts differ, the timing comparison is suspect until the mismatch is explained.
