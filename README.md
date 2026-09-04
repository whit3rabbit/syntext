<div align="center">
<pre>

███████╗██╗   ██╗███╗   ██╗████████╗███████╗██╗  ██╗████████╗
██╔════╝╚██╗ ██╔╝████╗  ██║╚══██╔══╝██╔════╝╚██╗██╔╝╚══██╔══╝
███████╗ ╚████╔╝ ██╔██╗ ██║   ██║   █████╗   ╚███╔╝    ██║
╚════██║  ╚██╔╝  ██║╚██╗██║   ██║   ██╔══╝   ██╔██╗    ██║
███████║   ██║   ██║ ╚████║   ██║   ███████╗██╔╝ ██╗   ██║
╚══════╝   ╚═╝   ╚═╝  ╚═══╝   ╚═╝   ╚══════╝╚═╝  ╚═╝   ╚═╝

</pre>

**A faster grep for agent loops.** 1.2x to 3.5x faster than ripgrep across five real repositories, and up to 18x on selective queries in the Linux kernel.

The speedup varies with query selectivity, and search time does not include the index build. A common token that hits most files runs about as fast as `rg`. See [Benchmarks](#benchmarks).

[![CI](https://github.com/whit3rabbit/syntext/actions/workflows/ci.yml/badge.svg)](https://github.com/whit3rabbit/syntext/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/syntext.svg)](https://crates.io/crates/syntext)
[![docs.rs](https://docs.rs/syntext/badge.svg)](https://docs.rs/syntext)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

[Install](#installation) • [Agent quick start](#quick-start-for-agents) • [Usage](#usage) • [Benchmarks](#benchmarks) • [Harnesses](#agent-harnesses) • [Architecture](#architecture) • [Docs](#docs)

</div>

`syntext` is a hybrid code search index for agent workflows, built in Rust. It indexes a repository with sparse n-grams, narrows each query to a small candidate set, and verifies the candidates against file bytes. The binary is `st`, and it accepts ripgrep's flags, so it drops into agent loops that call `rg` repeatedly and in parallel.

It is a local tool. The index lives in `.syntext/` with owner-only permissions, and every path it opens is checked to stay inside the repository (kernel-enforced on Linux via `openat2`, canonicalized elsewhere). The threat model and audit findings are in [docs/SECURITY.md](docs/SECURITY.md).

Agent harness installs edit your editor and agent config files. Each install writes a timestamped backup first.

## Installation

### Quick install (macOS and Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
```

Installs `st` to `/usr/local/bin`. On macOS it uses the Homebrew cask when `brew` is present and downloads the release zip otherwise. On Debian and Ubuntu (x86_64) it installs the `.deb` package. Every other Linux target gets the raw binary. All downloads are verified against the release's `SHA256SUMS`.

Override the install directory or pin a version with environment variables:

```bash
INSTALL_DIR=~/.local/bin SYNTEXT_VERSION=2.3.0 \
  curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
```

Every release also ships raw binaries for Linux (amd64, arm64), macOS (arm64, x86_64), and Windows, plus the `.deb`, on the [releases page](https://github.com/whit3rabbit/syntext/releases).

<details>
<summary>macOS (Homebrew)</summary>

```bash
brew tap whit3rabbit/tap
brew install --cask whit3rabbit/tap/syntext
```

</details>

<details>
<summary>Windows (PowerShell)</summary>

```powershell
iwr -useb https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.ps1 | iex
```

Installs `st.exe` to `%LOCALAPPDATA%\syntext` and adds it to the user `PATH`. Restart your terminal after install.

To run from a saved copy of the script:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```

</details>

<details>
<summary>WASM and Swift</summary>

Prebuilt WASM packages ship on the [releases page](https://github.com/whit3rabbit/syntext/releases) as `syntext-wasm-<version>.tar.gz`. To build from source:

```bash
cargo install wasm-pack
wasm-pack build --target bundler -- --features wasm --no-default-features
# output: pkg/  (JS glue + .wasm + TypeScript types)
```

Other targets: `--target nodejs`, `--target web`. The `wasm` feature builds a fully in-memory index with no filesystem access.

Swift bindings (macOS 12+) ship as `syntext-swift-<version>.xcframework.zip` and as the Swift package in [swift/](swift/). See [docs/SWIFT.md](docs/SWIFT.md).

</details>

### From source

```bash
cargo install syntext
```

## Quick start for agents

Three commands make an agent harness reach for `st` instead of `rg`. Run them with the `st` you intend to keep (the Homebrew or `/usr/local/bin` one), because the hook records that binary's absolute path.

```bash
# 1. Build the index in the repo the agent works in
st index

# 2. Install the integration for your harness
st init -g                # Claude Code: global Bash rewrite hook, Grep blocker, and rules
st init -g --codex        # Codex CLI: ~/.codex/SYNTEXT.md plus an @SYNTEXT.md include in ~/.codex/AGENTS.md
st init                   # Claude Code, project only: a rules block in this repo's CLAUDE.md
st init --codex           # Codex, project only: ./SYNTEXT.md plus an include in ./AGENTS.md

# 3. Confirm
st agent show claude --global     # prints "claude global: installed"
```

What the agent gets:

- **A rewrite hook** (Claude Code, Cursor, Gemini, Copilot, OpenCode, OpenClaw). When the agent runs `rg` or `grep` in a repo that has `.syntext/`, the hook rewrites the command to `st` with the same flags. Claude Code shows the rewrite as an "ask" so you see it before it runs. Commands the rewrite cannot reproduce faithfully (`-c`, `-v`, multiline, pipes, shell expansions) run exactly as typed.
- **A Grep blocker** (Claude Code only). The built-in Grep tool is denied with a reason pointing at `st`, again only when an index exists, so the agent falls through to the indexed search.
- **Rules text** (every harness). One shared block: use `st` when `.syntext/` exists, run `test -d .syntext || st index` before the first search, do not run `st update` after edits, and bound output with `--max-results`, `-l`, `-c`, `-m`, `-C`, `-g`, or `-t`.

Running an install twice changes nothing. Each install backs up the file it edits with a timestamp, and `st agent uninstall <name> --global` removes the entries syntext wrote and nothing else. Integrations without a hook surface (Codex, Cline, Windsurf, Kilo Code, Antigravity) get the rules text alone. The full matrix, with every file each install touches, is [below](#agent-harnesses).

## Usage

```bash
# Build the index once per repo. It lives in .syntext/ at the repo root
# (the nearest .git ancestor). Searches without an index fall back to rg.
st index
st index --stats                    # file count and index size after the build

# Override where the index is stored or which root to index
st --repo-root /path/to/repo index
st --index-dir /tmp/my-index index

# Search the whole repo. Search is the default command.
st "fn parse_query"                 # regex
st -F "parse_query("                # literal (metacharacters stay literal)
st -i "parsequery"                  # case-insensitive
st -S "parseQuery"                  # smart case: sensitive only if the pattern has uppercase
st -w "parse"                       # whole words only
st -x "TODO"                        # whole-line match
st -n "impl.*Iterator"              # force line numbers
st -e "foo" -e "bar"                # several patterns, OR-combined
st -f patterns.txt                  # patterns from a file, one per line
st -C 2 "fn main"                   # 2 lines of context either side (-A, -B for one side)

# Restrict search scope with positional paths
st "needle" src/                    # one directory
st "needle" src/lib.rs              # one file
st "needle" src/lib.rs tests/       # several files or directories

# Filters and output modes
st -t rs "impl.*Iterator"           # Rust files only (--rust is the same thing)
st -T md "TODO"                     # exclude a file type
st -g "src/**" "TODO"               # restrict by glob
st --exclude-dir node_modules "fn " # skip directories by name (grep compat)
st -c "parse_query" src/lib.rs      # count matching lines in one file
st -l "parse_query"                 # matching file paths only
st --files-without-match "TODO"     # files with zero matches
st -o "fn [a-z_]+"                  # only the matched text
st -m 3 "TODO"                      # at most 3 matching lines per file
st --max-results 20 "TODO"          # cap total output (files under -l)
st --vimgrep "TODO"                 # path:line:col:content, one match per line
st --json "TODO"                    # NDJSON output for tooling
st --files src/                     # list the indexed files in scope, no search

# Search a stream instead of the repo (no index needed)
cargo test 2>&1 | st "FAILED"       # implicit: stdin is a pipe or redirect
st "FAILED" -                       # explicit: `-` always means stdin
git log --oneline | st -c "fix"     # bare count, like rg

# Index maintenance
st status                           # documents, segments, and files behind
st status --json                    # the same, machine-readable
st update                           # apply and persist working-tree changes now
st verify                           # full checksum of every segment
st index --recalibrate              # re-measure the index-vs-scan crossover after a hardware change
```

`--index-dir` and `--repo-root` work on every subcommand, and `SYNTEXT_INDEX_DIR` is the env form of `--index-dir`.

### The index keeps itself fresh

Every search first asks git what changed since the index was built and applies those files to an in-memory overlay before searching. The default budget is 150 ms and 200 files, so a normal edit loop never needs `st update`.

Past the budget, the search runs on the stale index and prints `st: index is ~N files behind` to stderr. It also spawns a detached `st update --quiet` that persists the catch-up for the next process.

Git hooks installed by `st init --githooks` (project scope only) trigger the same background update on commit, checkout, merge, and rewrite. On a large repo, `st init --fsmonitor` turns on git's `core.fsmonitor` so the per-search change check is near-instant. It starts a git background daemon, so it is opt-in.

Tune or disable the behavior with `--no-update` (or `SYNTEXT_NO_AUTO_UPDATE=1`), `SYNTEXT_AUTO_UPDATE_BUDGET_MS`, `SYNTEXT_AUTO_UPDATE_MAX_FILES`, and `SYNTEXT_NO_ASYNC_UPDATE=1`.

### Notes

- Like ripgrep, file names print by default when searching a directory, the whole repo, or several positional paths.
- Like ripgrep, line numbers are off when stdout is not a TTY. Use `-n` to force them on.
- Stdin filtering follows ripgrep's rules. A pipe or `< file` redirect is searched when no paths are given (a tty, socket, or `/dev/null` is not), an explicit `-` always reads stdin, explicit path arguments win over stdin, and `-v` inverts per line on a stream.
- Stream output matches `rg` reading the same pipe: no filename prefix by default, `<stdin>` under `-H`, `-l`, or `--json`. `st 'pat' - src/` searches both the stream and the paths, with stdin results ordered by the argv position of `-`. Under `-v` that mix still exits 2.
- A pattern word that collides with a subcommand name (`st -F 'index'`) routes to that subcommand. Use `st -e 'index'` or `st -- 'index'` to search for it.

## Benchmarks

Search latency across five real repositories, averaged over each preset's token-aligned queries. Run on 11 July 2026 with the v2.0 release candidate on macOS, Apple Silicon, using [scripts/bench_compare.py](scripts/bench_compare.py) and the presets in [benchmarks/repo_presets.json](benchmarks/repo_presets.json).

| Repo | Tracked files | `st` avg | `rg` avg | `grep` avg | Speedup vs `rg` |
|---|---:|---:|---:|---:|---:|
| React | 2,447 | 38.2 ms | 44.2 ms | 152.2 ms | 1.2x |
| Rust compiler | 45,286 | 775.5 ms | 1,039.6 ms | 1,583.1 ms | 1.3x |
| TypeScript | 70,986 | 1,618.8 ms | 1,919.5 ms | 2,511.5 ms | 1.2x |
| Node.js | 40,812 | 704.0 ms | 912.4 ms | 2,429.0 ms | 1.3x |
| Linux kernel | 83,475 | 725.0 ms | 2,509.8 ms | n/a | 3.5x |

What these numbers do not cover:

- **They are averages.** The win is a function of query selectivity. On the Linux kernel, `raw_spin_lock` ran in 133 ms against 2,443 ms for `rg` (18x) and `irq_work_queue` in 186 ms against 2,498 ms (13x), while `sched_clock` ran in 1,856 ms against 2,588 ms (1.4x). A common token that hits most files is verification-bound and lands near `rg`.
- **Search time excludes the index build.** Building the Linux index took 4.6 s on this machine. The tool pays for itself when the same tree is searched many times, which is what an agent loop does.
- **The TypeScript row is suspect.** Both TypeScript queries in that run undercounted against `rg` (144 vs 181, and 191 vs 345) because they were mid-token substrings, so the timing there is not a like-for-like comparison. The other four presets had exact count parity.
- **Linux is faster than macOS.** On Linux the path containment check is a single `openat2(RESOLVE_BENEATH)` syscall. On macOS and Windows it is user-space canonicalization per file, which is the dominant per-candidate cost.

See [docs/BENCHMARKS.md](docs/BENCHMARKS.md) for methodology, per-query tables, build times, and historical runs.

## Fallback to ripgrep and grep

Searching a path with no index falls back to `ripgrep`, or to `grep` when `rg` is not on `PATH`, so a search in a throwaway clone under `/tmp` returns results instead of exit code 2. This is on by default. Disable it with `SYNTEXT_FALLBACK_RG=0` (also accepts `false`, `no`, `off`). The `--fallback` flag overrides the variable.

```bash
st "needle" /tmp/some-clone                          # default: rg fallback plus a notice
SYNTEXT_FALLBACK_RG=0 st "needle" /tmp/some-clone    # opt out (exit 2, no fallback)
st --fallback "needle" /tmp/some-clone               # force on despite the env var
```

- The fallback triggers **only** when the index is missing. A corrupt index or a lock conflict still fails loudly, so real problems are never masked.
- `ripgrep` receives your arguments unchanged. `st`'s CLI is a superset of `rg`'s, so `--json`, `--vimgrep`, context, and filter flags produce exactly the output `rg` would.
- `grep` is best-effort. Common match flags are mapped, and output-only modes grep cannot produce (`--json`, `--vimgrep`, `--heading`, `--column`, `-t`) are dropped.
- The fallback prints a one-line notice to stderr, suppressed under `--quiet` or `SYNTEXT_QUIET_FALLBACK=1`. Stdout stays clean for parsing.

Build an index for full speed and for syntext's coverage guarantees. The fallback is a convenience for un-indexed paths, not a replacement for `st index`.

## Agent harnesses

`st init` is the RTK-style front door. Hooks rewrite safe agent shell searches from `rg` or `grep` to `st`, and only when a `.syntext/` index exists. Human shells, scripts, pipes, CI, and unsupported search forms are left alone.

The shell-rewrite hooks never run `st index` or `st update` themselves. The separate git-hooks integration does run `st update --quiet` in the background after a commit, checkout, merge, or rewrite.

```bash
st init -g --agent cursor
st init -g --gemini
st init --copilot        # project hook; `st init -g --copilot` is also accepted
st init --githooks       # background `st update` from git hooks (project scope)
st init --fsmonitor      # opt in to git's core.fsmonitor for faster change detection
```

Explicit install, show, and uninstall commands are also available:

```bash
st agent install claude --global
st agent show claude --global
st agent uninstall claude --global
```

Supported harnesses:

| Harness | Scope | Install command | What is patched or written |
|---|---|---|---|
| Claude Code | global | `st init -g` or `st agent install claude --global` | `~/.claude/settings.json`, `~/.claude/SYNTEXT.md`, `~/.claude/CLAUDE.md` |
| Claude Code | project | `st init` or `st agent install claude --project` | `./CLAUDE.md` |
| Cursor | global | `st init -g --agent cursor` or `st agent install cursor --global` | `~/.cursor/hooks.json` |
| GitHub Copilot | project | `st init --copilot` or `st agent install copilot --project` | `./.github/hooks/syntext-rewrite.json`, `./.github/copilot-instructions.md` |
| Gemini CLI | global | `st init -g --gemini` or `st agent install gemini --global` | `~/.gemini/hooks/syntext-hook.sh`, `~/.gemini/settings.json`, `~/.gemini/GEMINI.md` |
| OpenCode | global | `st init -g --opencode` or `st agent install opencode --global` | `~/.config/opencode/plugins/syntext.ts` |
| OpenClaw | global | `st init -g --openclaw` or `st agent install openclaw --global` | `~/.openclaw/extensions/syntext-rewrite/` |
| Codex CLI | global or project | `st init -g --codex`, `st init --codex`, or `st agent install codex --global/--project` | `SYNTEXT.md` plus `AGENTS.md` include |
| Cline / Roo Code | project | `st init --cline` or `st agent install cline --project` | `./.clinerules` |
| Windsurf | project | `st init --windsurf` or `st agent install windsurf --project` | `./.windsurfrules` |
| Kilo Code | project | `st init --kilocode` or `st agent install kilocode --project` | `./.kilocode/rules/syntext-rules.md` |
| Google Antigravity | project | `st init --antigravity` or `st agent install antigravity --project` | `./.agents/rules/antigravity-syntext-rules.md` |
| Git hooks (auto-update) | project | `st init --githooks` or `st agent install githooks --project` | `.git/hooks/post-commit`, `post-checkout`, `post-merge`, `post-rewrite` |

Each install is idempotent, preserves unrelated settings, writes a timestamped backup before editing an existing file, and removes only syntext-owned entries on uninstall.

## Architecture

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

Three index components:

- **Content index**: sparse n-gram posting lists. Context-independent forced boundaries mean no false negatives for token-aligned queries.
- **Path index**: Roaring bitmap component sets for path and type filtering.
- **Symbol index** (optional, `symbols` feature): Tree-sitter extraction into SQLite.

Segments are immutable single-file mmap structures (SNTX format). Updates commit atomically to an in-memory overlay via `ArcSwap`. Durable incremental updates, from a moved HEAD or from `st update`, are written as LSM-style delta segments with a checksummed delete-set sidecar.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the quantitative analysis: selectivity math, index size estimates, and posting list encoding tradeoffs.

## Known limitations

1. **Crash recovery.** An overlay that has not been flushed is in memory only, so a resident integration loses uncommitted overlay state on an unclean shutdown. The CLI is durable: `st update` and the detached catch-up both persist what they apply as delta segments and delete sidecars. If the delete-set sidecar is corrupted, the index fails closed and needs `st index` or `st update`.
2. **Non-aligned substring coverage.** About 16% false negatives for queries that do not align with token boundaries, measured by property-based fuzzing. Token-aligned queries (identifiers, keywords) have 0% false negatives.
3. **Network filesystems.** The index directory must be on a local filesystem. NFS and SMB behavior is undefined.
4. **Case-insensitive overhead.** About 15 to 20% more candidates because of lowercase normalization. The verifier guarantees correct results.
5. **`\r`-only line endings.** Treated as a single line, matching ripgrep.
6. **Symbol search accuracy.** Tier 3 (heuristic) results are approximate, and Tree-sitter failures fall back silently.
7. **One root per index.** Each index covers exactly one `--repo-root`. Searching across two repos means building two indexes and querying each separately. `st update` needs a git repo, so a non-git directory is refreshed with `st index`.

## Docs

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): selectivity math, index size estimates, posting list encoding, design tradeoffs
- [docs/BENCHMARKS.md](docs/BENCHMARKS.md): methodology, per-query tables, and historical runs
- [docs/SECURITY.md](docs/SECURITY.md): threat model, audit findings, and accepted risks
- [docs/SWIFT.md](docs/SWIFT.md): Swift bindings and the C ABI
- [docs/COMPARISON_FFF.md](docs/COMPARISON_FFF.md) and [docs/COMPARISON_ZVEC.md](docs/COMPARISON_ZVEC.md): how syntext differs from fff and zvec-grep
- [docs/RELEASE.md](docs/RELEASE.md): the release checklist

## License

MIT. See [LICENSE](LICENSE).
