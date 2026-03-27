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

## Caveats

- This is not the Linux kernel. It is only the largest suitable local repo that
  was already available on this machine.
- `ripline` search latency excludes index build time. Index build is reported
  separately because `rg` and `grep` are no-index tools.
- If we later benchmark a kernel checkout, use the same script and record the
  command and corpus details here instead of mixing results across different
  repos.
