<div align="center">
<pre>

███████╗██╗   ██╗███╗   ██╗████████╗███████╗██╗  ██╗████████╗
██╔════╝╚██╗ ██╔╝████╗  ██║╚══██╔══╝██╔════╝╚██╗██╔╝╚══██╔══╝
███████╗ ╚████╔╝ ██╔██╗ ██║   ██║   █████╗   ╚███╔╝    ██║
╚════██║  ╚██╔╝  ██║╚██╗██║   ██║   ██╔══╝   ██╔██╗    ██║
███████║   ██║   ██║ ╚████║   ██║   ███████╗██╔╝ ██╗   ██║
╚══════╝   ╚═╝   ╚═╝  ╚═══╝   ╚═╝   ╚══════╝╚═╝  ╚═╝   ╚═╝

**A faster grep for agentic AI. Up to 20x+ faster than ripgrep on LARGE codebases.**

</pre>

[![CI](https://github.com/whit3rabbit/syntext/actions/workflows/ci.yml/badge.svg)](https://github.com/whit3rabbit/syntext/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/syntext.svg)](https://crates.io/crates/syntext)
[![docs.rs](https://docs.rs/syntext/badge.svg)](https://docs.rs/syntext)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

</div>

Hybrid code search index for agent workflows, built in Rust. Indexes repositories using sparse n-grams, then narrows to a small candidate set before verification. Drop-in replacement for `rg` in AI agent loops where grep is called repeatedly and in parallel.

**Status: stable (v2.0).**

## Installation

### Quick install (macOS and Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
```

Installs `st` to `/usr/local/bin`. On macOS, uses Homebrew cask if `brew` is available. On Debian/Ubuntu (x86_64), installs the `.deb` package. All other Linux targets get the raw binary. Checksums are verified against `SHA256SUMS` from the release.

Override defaults with environment variables:

```bash
INSTALL_DIR=~/.local/bin SYNTEXT_VERSION=2.0.0 \
  curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
```

<details>
<summary>macOS (Homebrew)</summary>

```bash
brew tap whit3rabbit/tap
brew install --cask whit3rabbit/tap/syntext
```

</details>

<details>
<summary>Linux (manual)</summary>

```bash
VERSION=2.0.0

# Debian/Ubuntu (x86_64)
curl -L "https://github.com/whit3rabbit/syntext/releases/download/v${VERSION}/syntext_${VERSION}_amd64.deb" \
  -o "syntext_${VERSION}_amd64.deb"
sudo dpkg -i "syntext_${VERSION}_amd64.deb"

# Any Linux (x86_64 or arm64)
ARCH=amd64   # or arm64
curl -L "https://github.com/whit3rabbit/syntext/releases/download/v${VERSION}/st-${VERSION}-linux-${ARCH}" -o st
chmod +x st && sudo mv st /usr/local/bin/
```

</details>

<details>
<summary>Windows (PowerShell)</summary>

```powershell
iwr -useb https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.ps1 | iex
```

Installs `st.exe` to `%LOCALAPPDATA%\syntext` and adds it to the user `PATH`. Restart your terminal after install.

To pin a version or run from a saved script:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```

</details>

<details>
<summary>WASM</summary>

Prebuilt WASM packages are available on the [releases page](https://github.com/whit3rabbit/syntext/releases) as `syntext-wasm-<version>.tar.gz`. To build from source:

```bash
cargo install wasm-pack
wasm-pack build --target bundler -- --features wasm --no-default-features
# output: pkg/  (JS glue + .wasm + TypeScript types)
```

Other targets: `--target nodejs`, `--target web`.

</details>

### From source

```bash
cargo install syntext
```

## Benchmarks

Search latency across five real-world repositories (v2.0, macOS, Apple Silicon).

| Repo | `st` avg | `rg` avg | `grep` avg | Speedup vs `rg` |
|---|---:|---:|---:|---:|
| React | `38.2 ms` | `44.2 ms` | `152.2 ms` | `1.2x` |
| Rust compiler | `775.5 ms` | `1039.6 ms` | `1583.1 ms` | `1.3x` |
| TypeScript | `1618.8 ms` | `1919.5 ms` | `2511.5 ms` | `1.2x` |
| Node.js | `704.0 ms` | `912.4 ms` | `2429.0 ms` | `1.3x` |
| Linux kernel | `725.0 ms` | `2509.8 ms` | n/a | `3.5x` |

Average speedup across five presets: **1.7x** versus `rg`. Search time excludes index build time.

> [!NOTE]
> Speedup is most significant on **large repositories** and **selective queries** where the index eliminates the need to scan tens of thousands of files. Performance is also substantially faster on **Linux** than macOS; Linux utilizes kernel-level `openat2(RESOLVE_BENEATH)` for secure path containment, completely bypassing the user-space canonicalization and metadata check overhead required on macOS.

See [docs/BENCHMARKS.md](docs/BENCHMARKS.md) for methodology, index build times, query discipline, and historical runs.

## Usage

```bash
# Build the index (run once per repo, then only after large changes)
# Index is stored in .syntext/ at the repo root (nearest .git ancestor).
# Not run automatically -- you must run this before the first search.
st index
st index --stats                    # show file count and index size after build

# Override where the index is stored or which root to index
st --repo-root /path/to/repo index
st --index-dir /tmp/my-index index

# After editing files, sync the index incrementally (faster than full rebuild)
st update

# Search the whole repo (index must exist)
st "fn parse_query"                 # regex
st -F "parse_query("                # literal (metacharacters stay literal)
st -i "parsequery"                  # case-insensitive
st -x "TODO"                        # whole-line match
st -n "impl.*Iterator"              # force line numbers

# Restrict search scope with positional paths
st "needle" src/                    # search one directory
st "needle" src/lib.rs              # search one file
st "needle" src/lib.rs tests/       # search multiple files/directories

# Additional filters and output modes
st -t rs "impl.*Iterator"           # restrict to Rust files
st -g "src/" "TODO"                 # restrict by glob
st -c "parse_query" src/lib.rs      # count matches in one file
st -l "parse_query"                 # print matching file paths
st --json "TODO"                    # NDJSON output for tooling

# Status
st status
```

Notes:

- Search is the default command, there is no `st search` subcommand.
- Like ripgrep, file names are shown by default when searching a directory, the whole repo, or multiple positional paths.
- Like ripgrep, line numbers are off by default when stdout is not a TTY. Use `-n` to force them on.

## Fallback to ripgrep/grep (un-indexed search)

By default, searching a path with no index fails with exit code 2 and tells you
to run `st index`. For agent harnesses that sometimes search outside an indexed
checkout (e.g. a throwaway clone in `/tmp`), `st` can instead fall back to
`ripgrep` (preferred) or `grep` so the search still returns results.

It is opt-in. Enable it with the `--fallback` flag or `SYNTEXT_FALLBACK_RG=1`
(accepts `1`, `true`, `yes`, `on`):

```bash
SYNTEXT_FALLBACK_RG=1 st "needle" /tmp/some-clone   # env var (set once in harness)
st --fallback "needle" /tmp/some-clone              # per-invocation flag
```

Behavior:

- Triggers **only** when the index is missing. A corrupt index or lock conflict
  still fails loudly so real problems are never masked.
- `ripgrep` receives your original arguments unchanged (st's CLI is a superset of
  rg's), so `--json`, `--vimgrep`, context, and filter flags produce exactly the
  output you would get from rg directly.
- `grep` is the last resort when `rg` is not on `PATH`. It is best-effort:
  common match flags are mapped, but output-only modes that grep cannot produce
  (`--json`, `--vimgrep`, `--heading`, `--column`, `-t/--type`) are dropped.
- The fallback is slower than the index and prints a one-line notice to stderr
  (suppressed under `--quiet`); stdout is left clean for parsing.

This is a convenience for un-indexed paths, not a replacement for `st index`:
build an index for full speed and syntext's coverage guarantees.

## Agent harness install

`st` can install RTK-style agent harness integrations. Programmatic hooks rewrite
safe agent shell searches from `rg` or `grep` to `st` only when a `.syntext/`
index exists. Human shells, scripts, pipes, CI, and unsupported search forms are
left alone. Hooks never run `st index` or `st update` automatically.

Quick installs:

```bash
# Claude Code project instructions only
st init

# Claude Code global Bash hook plus Grep blocker
st init -g

# RTK-style agent selectors
st init -g --agent cursor
st init -g --gemini
st init --copilot        # project hook; `st init -g --copilot` is also accepted
st init --codex          # project rules
st init -g --codex       # global Codex rules
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

Each install is idempotent, preserves unrelated settings, writes a timestamped
backup before editing an existing file, and only removes syntext-owned entries
on uninstall.

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

- **Content index**: sparse n-gram posting lists. Context-independent forced boundaries ensure no false negatives for token-aligned queries.
- **Path index**: Roaring bitmap component sets for path/type filtering.
- **Symbol index** (optional): Tree-sitter extraction into SQLite.

Segments are immutable single-file mmap structures (SNTX format). Updates commit atomically to an in-memory overlay via `ArcSwap`, while durable incremental HEAD-move updates are written as LSM-style delta segments with a checksummed delete-set sidecar.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full quantitative analysis: selectivity math, index size estimates, posting list encoding tradeoffs.

## WASM

The `wasm` Cargo feature compiles syntext to a fully in-memory index with no filesystem access. See the [releases page](https://github.com/whit3rabbit/syntext/releases) for prebuilt `syntext-wasm-<version>.tar.gz`, or build from source:

```bash
wasm-pack build --target bundler -- --features wasm --no-default-features
# output: pkg/  (JS glue + .wasm + TypeScript types)
```

## Known limitations

1. **Crash recovery**: Uncommitted in-memory overlay state (used by resident integrations) is lost on unclean shutdown. For CLI searches, index state is persisted to disk via delta segments and delete sidecars, and any staleness is auto-healed on the next search via automatic bounded update-on-search. If a sidecar is corrupted, the index fails closed and requires a re-index or update.
2. **Non-aligned substring coverage**: ~16% false-negative rate for queries that don't align with token boundaries. Token-aligned queries (identifiers, keywords) have 0% false negatives.
3. **Network filesystems**: Index directory must be on local filesystem. NFS/SMB behavior is undefined.
4. **Case-insensitive overhead**: ~15-20% more candidates due to lowercase normalization. Correct results are guaranteed by the verifier.
5. **`\r`-only line endings**: Treated as a single line (matches ripgrep behavior).
6. **Symbol search accuracy**: Tier 3 (heuristic) results are approximate. Tree-sitter failures fall back silently.
7. **One root per index**: Each index covers exactly one `--repo-root`. There is no way to merge multiple directories into a single index. To search across two repos, build and query each index separately with `--repo-root`. `st update` requires a git repo; non-git directories must be re-indexed with `st index`.

## Design documents

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** -- Quantitative analysis: selectivity math, index size estimates, posting list encoding, design tradeoffs

## License

MIT
