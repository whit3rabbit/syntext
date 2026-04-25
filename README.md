# syntext

[![CI](https://github.com/whit3rabbit/syntext/actions/workflows/ci.yml/badge.svg)](https://github.com/whit3rabbit/syntext/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/syntext.svg)](https://crates.io/crates/syntext)
[![docs.rs](https://docs.rs/syntext/badge.svg)](https://docs.rs/syntext)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

**A faster grep for agentic AI. ~20X faster than ripgrep when indexed.**

Hybrid code search index for agent workflows, built in Rust. Indexes repositories using sparse n-grams, then narrows to a small candidate set before verification. Drop-in replacement for `rg` in AI agent loops where grep is called repeatedly and in parallel.

**Status: stable (v1.0).**

## Installation

### Quick install (macOS and Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/whit3rabbit/syntext/main/install.sh | sh
```

Installs `st` to `/usr/local/bin`. On macOS, uses Homebrew cask if `brew` is available. On Debian/Ubuntu (x86_64), installs the `.deb` package. All other Linux targets get the raw binary. Checksums are verified against `SHA256SUMS` from the release.

Override defaults with environment variables:

```bash
INSTALL_DIR=~/.local/bin SYNTEXT_VERSION=1.0.1 \
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
VERSION=1.0.1

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

Search latency across five real-world repositories (v1.0, macOS, Apple Silicon).

| Repo | `st` avg | `rg` avg | `grep` avg | Speedup vs `rg` |
|---|---:|---:|---:|---:|
| React | `20.7 ms` | `112.9 ms` | `314.3 ms` | `5.5x` |
| Rust compiler | `99.9 ms` | `2183.2 ms` | `2412.8 ms` | `21.9x` |
| TypeScript | `111.9 ms` | `3093.8 ms` | `3171.8 ms` | `27.7x` |
| Node.js | `69.5 ms` | `1492.6 ms` | `3186.4 ms` | `21.5x` |
| Linux kernel | `154.5 ms` | `3681.3 ms` | n/a | `23.8x` |

Average speedup across five presets: **20.1x** versus `rg`. Search time excludes index build time.

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

- **Content index**: sparse n-gram posting lists. Trigram augmentation ensures no false negatives for token-aligned queries.
- **Path index**: Roaring bitmap component sets for path/type filtering.
- **Symbol index** (optional): Tree-sitter extraction into SQLite.

Segments are immutable single-file mmap structures (SNTX format). Updates go through an in-memory overlay with atomic batch commit via `ArcSwap`.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full quantitative analysis: selectivity math, index size estimates, posting list encoding tradeoffs.

## WASM

The `wasm` Cargo feature compiles syntext to a fully in-memory index with no filesystem access. See the [releases page](https://github.com/whit3rabbit/syntext/releases) for prebuilt `syntext-wasm-<version>.tar.gz`, or build from source:

```bash
wasm-pack build --target bundler -- --features wasm --no-default-features
# output: pkg/  (JS glue + .wasm + TypeScript types)
```

## Project status

**All phases complete (v1.0).** Core `st index && st "pattern"` workflow validated against ripgrep. Symbol search available behind `--features symbols`.

| Phase | Status | What it delivers |
|---|---|---|
| 1. Setup | Complete | Cargo project, dependencies, module structure |
| 2. Foundational | Complete | Weight table, tokenizer, posting lists, correctness harness |
| 3. US5 -- Build | Complete | Full index build from scratch |
| 4. US1 -- Search | Complete | Literal + regex search, ripgrep correctness validation |
| 5. US2 -- Incremental | Complete | Overlay, batch commit, read-your-writes |
| 6. US3 -- Path scoping | Complete | Path/type filters with Roaring bitmaps |
| 7. US4 -- Symbols | Complete | Tree-sitter symbol extraction, SQLite storage |
| 8. CLI | Complete | `st` binary with grep-compatible output |
| 9. Polish | Complete | Bug fixes, security hardening, benchmarks, documentation |

## Known limitations

1. **Crash recovery**: Overlay state is lost on unclean shutdown. Run `st update` or `st index` after a crash.
2. **Non-aligned substring coverage**: ~16% false-negative rate for queries that don't align with token boundaries. Token-aligned queries (identifiers, keywords) have 0% false negatives.
3. **Network filesystems**: Index directory must be on local filesystem. NFS/SMB behavior is undefined.
4. **Case-insensitive overhead**: ~15-20% more candidates due to lowercase normalization. Correct results guaranteed by verifier.
5. **`\r`-only line endings**: Treated as a single line (matches ripgrep behavior).
6. **Symbol search accuracy**: Tier 3 (heuristic) results are approximate. Tree-sitter failures fall back silently.
7. **One root per index**: Each index covers exactly one `--repo-root`. There is no way to merge multiple directories into a single index. To search across two repos, build and query each index separately with `--repo-root`. `st update` requires a git repo; non-git directories must be re-indexed with `st index`.

## Design documents

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** -- Quantitative analysis: selectivity math, index size estimates, posting list encoding, design tradeoffs
- **[specs/001-hybrid-code-search-index/spec.md](specs/001-hybrid-code-search-index/spec.md)** -- Feature specification with user stories and acceptance criteria
- **[specs/001-hybrid-code-search-index/research.md](specs/001-hybrid-code-search-index/research.md)** -- 19-section architecture research covering every subsystem
- **[specs/001-hybrid-code-search-index/data-model.md](specs/001-hybrid-code-search-index/data-model.md)** -- Entity definitions and relationships
- **[specs/001-hybrid-code-search-index/contracts/](specs/001-hybrid-code-search-index/contracts/)** -- Library API, CLI, and segment format contracts
- **[specs/001-hybrid-code-search-index/tasks.md](specs/001-hybrid-code-search-index/tasks.md)** -- Implementation plan with dependency graph

## License

MIT
