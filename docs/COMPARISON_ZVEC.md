# Comparison: syntext vs zvec-grep

Reference: [zvec-grep v0.2.1](https://github.com/zvec-ai/zvec-grep/tree/v0.2.1)
(Apache-2.0). Pinned to v0.2.1 because the project is under
[active development](https://github.com/zvec-ai/zvec-grep/blob/v0.2.1/docs/08-roadmap.md)
and surface contracts (CLI flags, MCP tool names, embedding catalog, server
endpoints) change between releases. The repo link itself is stable. This
document is rewritten when any of the compared axes shift.

This document records what zvec-grep does differently, what syntext adopted
from the comparison, and what it deliberately rejected. Every claim about
zvec-grep below was read off the v0.2.1 tree, and where a number is only
published as a chart image rather than as text, this document says so.
syntext-side latency numbers belong in `docs/BENCHMARKS.md` and are not
duplicated here.

Facts about both projects were last verified on 3 September 2026.

## What zvec-grep is

zvec-grep is a **long-running search layer plus CLI plus MCP server**, aimed
at agents and humans who want one workspace-anchored search surface that
covers exact lookup, ranked lexical, and semantic retrieval:

- **Local-first by default**, with explicit per-workspace authorization for
  remote embedding (`zg auth grant --capability embedding --scope workspace`).
  Files, indexes, and local models stay on disk. The server listens on
  loopback only (`127.0.0.1:7999`).
- **Hybrid retrieval fused via reciprocal rank fusion (RRF):** BM25 lexical
  candidates and dense-vector candidates combined into one ranked list
  (`RRF_K = 60`). Optional `--hybrid` / `--fts` / `--vector` query groups
  with `--fuse`. Without `--fuse` the groups are returned side by side and
  are not deduplicated against each other.
- **Mandatory embedding model.** 14 catalog entries at v0.2.1, 11 under
  `local/` and 3 under `qwen/` (`local/potion-code-16m-v2`,
  `local/potion-retrieval-32m`, `local/jina-embeddings-v2-base-code`,
  `qwen/qwen3.7-text-embedding`, and so on), ranging from a static Model2Vec
  at 256 dimensions to a remote 128K-context Qwen. Parameter counts appear
  only inside the upstream model names, the catalog itself records
  dimension. Model revisions are pinned by the release.
- **Daemon with three execution modes:** `auto` (use ready server or fall back
  to direct), `server` (require daemon), `direct` (one-shot, no port). The
  daemon coordinates background index refresh, watcher events, and shared
  loaded model state across requests.
- **Managed ripgrep as an explicit escape hatch** (`zg query --rg`), which
  runs without an index or embedding model. Used for exact identifiers,
  regexes, filenames, and paths.
- **Managed agent integration** for six agents (`zg install --target codex|
  claude|qwen|qoder|opencode|cursor`). Writes MCP entries, injects guidance
  text between `ZVEC_GREP_START` / `ZVEC_GREP_END` markers, configures tool
  approval where supported, and starts the server.
- **MCP tool surface is intentionally narrow by default:** the `agent`
  toolset exposes only `zvec_grep_search`. `--mcp-toolset full` adds
  `zvec_grep_rg`, `zvec_grep_index`, `zvec_grep_index_drop`,
  `zvec_grep_index_status`, and `zvec_grep_server_status`. The shape forces
  agents to make a workspace-versus-native decision per query instead of
  falling back to grep.
- **Bounded result shape.** `DEFAULT_LIMIT = 7`, MCP caps at 50, and
  `--preview none|short|full` controls how much of each hit is rendered.
  Every hit carries `startLine`-`endLine`, an `outline` naming the enclosing
  symbol (tree-sitter WASM, 8 languages), and a `freshness` value.
- **Closed index format.** `.zvec` files produced by the
  [zvec](https://github.com/alibaba/zvec) library, pinned at `^0.7.0`. The
  manifest stores metadata and embedding runtime settings including, when
  persisted, an API key.

## Architectural difference

The core split is the retrieval model and where the index lives:

| | zvec-grep | syntext |
|---|---|---|
| Process model | Long-running daemon (auto / server / direct) | Stateless CLI (`st`), on-disk index, mmap per process |
| State location | `<workspace>/.zvec-grep/` (index) plus `~/.zvec-grep/` (daemon, config, model cache) | `<workspace>/.syntext/` (segments, manifest, sidecars) |
| Cold start | Index open plus optional model load plus daemon handshake | `Index::open`: mmap plus O(1) structural checks (milliseconds) |
| Warm query | Server route sub-ms against loaded index, direct route process start plus open plus query | `syntext-persistent` sub-ms, `syntext-fork` ~3 ms process start plus open plus query |
| Retrieval model | BM25 (jieba tokenizer) plus dense vector plus RRF plus managed rg | Sparse n-gram prefilter, then memchr/regex verify. No ranking, no fusion |
| Embedding model | Required (local or remote) | None |
| Freshness | FS watcher (`node:fs.watch`, 750 ms debounce) plus hourly reconcile, `freshness: possibly_stale` when drift is detected | Bounded update on every search (150 ms / 200 files), git-commit delta segments, staleness notice on stderr, four git post-hooks |
| Query language | Hybrid / FTS / vector / fused plus rg escape hatch | Literal plus regex, exact rg-compatible output |
| Output | Ranked passages with metadata, previews, enclosing symbol, freshness flag | rg-compatible lines (default, count, json, vimgrep), exhaustive and deterministic |
| Integration | MCP server (Streamable HTTP), managed install into six agents | CLI (`st`), Rust library, WASM, Swift FFI, optional Tree-sitter symbol index, managed install into 11 agent harnesses plus git hooks |
| Index format | `.zvec` (closed, zvec library output) | SNTX (open, documented in `docs/ARCHITECTURE.md`) |
| Index size on ~500 MB corpus | Not published. Embedding plus BM25 plus manifest will be many times larger | ~44 MB (0.09x) |
| License | Apache-2.0 | MIT |
| Adoption signal (3 September 2026) | 1,896 stars, 88 forks, Alibaba-backed, repo created 10 July 2026 | Personal project |

zvec-grep optimizes for the long-running agent-host case where the same
process answers many queries and a loaded model is shared across them.
syntext optimizes for the stateless-CLI case where every `st` invocation is a
fresh process and the only persistent state is on disk.

## What zvec-grep does better

- **Semantic search is a real capability syntext doesn't have.** "Where is
  async cancellation implemented?" and "What's the architectural pattern for
  X?" are unanswerable by exact-match grep at all. zvec-grep answers them via
  dense vectors. Their SWE-QA-Bench run (N=20, Claude Code plus Opus 5 high,
  3 trials per task, USD 4.00 cap per task) measures this end to end against
  a baseline agent, on repository questions ranging from cross-file data flow
  (matplotlib's `FontInfo` propagation) to design rationale (Django's
  username uniqueness interaction with formset transactions). Note that the
  published result numbers are chart images. The repo text gives the
  methodology but not the figures, so nothing here restates a score.
- **The "already-read evidence" instruction.** zvec-grep's injected guidance
  tells the agent to treat a sufficient snippet as read and skip the
  follow-up file read. That single line is likely a large share of their
  reported token win, and it costs nothing to implement. syntext adopted it
  (see *Adopt: rewritten agent guidance* below).
- **Bounded, structured result shape.** A default of 7 hits, a hard MCP cap
  of 50, three preview widths, and an enclosing-symbol field per hit. An
  agent gets a small answer by default and asks for more deliberately.
  syntext now has the cap (`--max-results`, below) but not the default, the
  preview widths, or the enclosing symbol. Exhaustive-by-default is the right
  choice for a shell pipeline and the wrong one for a naive agent call on a
  common token, which is why the cap is paired with guidance telling the agent
  to use it.
- **RRF-fused hybrid queries.** `{query, queries, fts, vector, fuse}` is a
  more flexible query shape than syntext's literal-versus-regex binary, and
  the per-group result types carry enough metadata that the agent can decide
  whether to follow up with a native rg call.
- **Provenance and version pinning.** Exact model revisions are pinned per
  release so the same reference resolves consistently. They had to solve this
  because vector spaces are model-coupled. An index built with
  `local/potion-code-16m-v2` is not queryable with
  `local/jina-embeddings-v2-base-code` even at the same dimensionality.
  syntext sidesteps the problem by having no model. If a model is ever added,
  the problem arrives intact.
- **Benchmark rigor at the agent level.** SWE-QA-Bench (N=20, paired A/B with
  Opus 5 high) and BrowseComp-Plus (N=100, paired A/B with gpt-5.6-sol) with
  a strict protocol, where task, agent, model, environment, prompt, and
  limits are held constant and only "zg access" varies, is a better shape
  than syntext's "5 repos x 5 queries, 1.7x vs rg" wall-clock comparison.
  zvec-grep is measuring the thing that matters for the agent pitch, syntext
  is measuring grep latency. One caveat on their side: the top-level README
  records the BrowseComp-Plus reasoning effort as `medium` and the benchmark
  README records it as `high`. The two disagree.
- **Residual integration gap.** syntext has no MCP server, so it has no MCP
  entry to register and no per-tool approval to configure. `zg install` also
  starts the daemon, which syntext has nothing equivalent to because there is
  no daemon. That is the honest remainder after `st agent install` (below),
  and it is a consequence of the process model, not an oversight.

## What syntext does better

- **Token-aligned coverage invariant with formal validation.** `proptest`
  with 5,000 structured cases and `cargo-fuzz` with 1.45M arbitrary byte
  sequences validate that every gram emitted by `build_covering(Q)` appears in
  `build_all(D)` for token-aligned queries, with zero violations
  (`docs/ARCHITECTURE.md` § *Key invariant*). The non-aligned substring gap
  (~16% violation rate for arbitrary substrings inside tokens) is documented,
  bounded, and acceptable for the agent use case, since token-aligned is the
  99% case. zvec-grep has no equivalent claim. BM25 is probabilistic, so it
  carries no false-negative guarantee, and managed rg is exhaustive but has
  no prefilter speedup. The coverage invariant is the strongest design point
  in syntext's docs and zvec-grep has nothing comparable.
- **Managed agent install with no runtime to install.** `st init` and
  `st agent install|uninstall|show` cover 11 agent harnesses (Claude, Cursor,
  Copilot, Gemini, OpenCode, OpenClaw, Codex, Cline, Windsurf, KiloCode,
  Antigravity) plus a git-hooks vendor, at global or project scope. Edits are
  guarded by `<!-- syntext-agent:<id>:start -->` markers, are idempotent,
  take timestamped backups, and remove only their own block on uninstall.
  zvec-grep covers six targets and needs Node to do it. syntext ships a
  static binary, which is also why their roadmap lists a non-npm install path
  as an open direction.
- **No daemon, no port, no model, no auth.** `st query "fn parse_query"` is
  one process. No listening socket, no daemon logs, no GPU, no model
  download, no model-version pinning question, no remote-permission grant to
  think about. For "ship a grep into CI, a container, or an embedded Swift
  app" this is the simpler surface. For "embed into a long-lived editor or
  agent" zvec-grep's server model wins.
- **Open, inspectable index format.** SNTX is documented in
  `docs/ARCHITECTURE.md` (`Segment format` section). `.zvec` is a closed
  Alibaba library output. If you need to debug, port, or fork the indexer,
  syntext is tractable. zvec-grep is coupled to the zvec library and to its
  release cadence.
- **rg-compatible line output.** Drops into shell pipelines and into any
  agent that already parses ripgrep. zvec-grep's output is its own
  ranked-passage shape, which is fine for its own MCP client but is friction
  for shell pipelines and for any agent not specifically taught to parse it.
  The count parity with rg/grep is the harness's correctness anchor (SC-004).
  Ranking breaks that anchor, so exhaustive output is a deliberate choice,
  not a missing feature.
- **Smaller index, faster cold start.** 44 MB on a 500 MB corpus (0.09x), and
  `Index::open` is mmap plus O(1) structural checks. zvec-grep's cold-start
  cost is index size plus, for transformer models, model load, plus, for
  server mode, a daemon handshake. The published numbers don't show
  syntext-versus-zvec head to head, but the orders of magnitude differ.
- **Bounded latency on the freshness path.** zvec-grep's MCP tool timeout
  defaults to 600 seconds, so a search call can block on indexing for ten
  minutes. syntext's update-on-search is budgeted at 150 ms and 200 files,
  after which the search runs anyway and prints a files-behind notice on
  stderr. Slow and correct is a defensible choice for a daemon, but an agent
  that has to wait is an agent that has already lost the token argument.
- **Documented design lineage with explicit comparison docs.**
  `docs/ARCHITECTURE.md` references Cursor's fast regex search with a 6-axis
  diff table and a *Potential improvements inspired by Cursor* section.
  `docs/COMPARISON_FFF.md` does the same for fff. zvec-grep has a roadmap
  doc but no comparable "vs X" documents. Its lineage (BM25 plus vectors plus
  Alibaba's zvec library) is implicit.
- **Forced-boundary context-independence.** Boundary detection at whitespace,
  operators, brackets, underscore, and control characters always splits
  regardless of neighbor bytes. The trained weight table only subdivides
  within alphanumeric spans. This eliminates an entire class of false
  negatives for token-aligned queries, because the boundary set at query time
  is guaranteed to be a subset of the boundary set at index time. zvec-grep's
  BM25 tokenization has no analogous invariant. It ranks, it does not promise
  to find all occurrences. It also runs the `jieba` tokenizer over code
  identifiers, which is a Chinese word-segmentation model doing a job it was
  not trained for.
- **WASM and Swift FFI surfaces.** The `wasm` and `ffi` feature flags ship an
  in-memory `WasmIndex` and a C-ABI surface with a Swift package on top of
  it (`docs/SWIFT.md`). zvec-grep is Node-only and has no WASM or FFI story.
- **No model download required for the core path.** A fresh `st index` on
  any repo works without fetching hundreds of MB of model weights, doesn't
  need a GPU or a Metal backend, and produces deterministic output
  independent of model revisions. This matters for CI, air-gapped
  environments, and reproducible benchmarks.

## Where the two designs agree

- **Local-first by default.** No remote call without explicit grant
  (zvec-grep via `zg auth grant`, syntext has no remote surface at all).
- **Workspace index anchored at root.** Both ignore `.git/` (zvec-grep also
  ignores `.zvec-grep`), and both scope by glob and type. zvec-grep supports
  `-g/--glob`, `-t/--type`, and `--max-filesize` with type-aware defaults
  (1 MiB code, 256 MiB text, 16 MiB structured data, 10 MiB images).
- **Ripgrep as escape hatch.** zvec-grep exposes `--rg`, syntext IS rg for
  output. Both honor the workspace conventions.
- **Drop-in for "agent calls grep repeatedly" pain.** zvec-grep pitches it
  as "fewer tool calls, fewer tokens", syntext pitches it as "1.7x wall
  time vs rg across five real repos". Same problem, different optimization
  target.

## Decisions

### Adopt: rewritten agent guidance

- **Decision (shipped):** Replace the injected guidance in
  `src/hook/core/instructions.rs` with text that tells the agent to bound its
  own output and to treat a returned line with context as already-read
  evidence, and that removes the "run `st update` after edits" instruction.
- **Rationale:** The old text cost a tool call per edit for something the
  search path already does on its own, since bounded update-on-search
  refreshes from git on every call. It also said nothing about output size,
  which is the failure mode that actually burns an agent's context when a
  common token matches 4,000 lines. zvec-grep's guidance gets both right, and
  copying the shape of an instruction is free.

### Adopt: `--max-results N`, a total output cap

- **Decision (shipped):** `st --max-results N` stops after N results across all
  files. `-m/--max-count` is per file, which does not bound total output, and
  nothing else did either. When output is cut short, a notice goes to stderr
  and the `--json` summary gains `"truncated": true`. Under `-l` the unit is
  files rather than matching lines, because that is what `-l` prints.
- **Rationale:** zvec-grep defaults to 7 hits and caps its MCP surface at 50.
  syntext's exhaustive output is correct for a shell pipeline and hostile to a
  naive agent call on a common token. A flag the agent can reach for, plus the
  guidance telling it to, is the version of that idea that does not break rg
  parity for everyone else.
- **What this is not:** not ranking. The first N results are the first N in
  path order, not the N best. There is no scoring model to be the best by.
- **Refused rather than ignored:** `-c`, `--count-matches`, `-v`, `-L`, and
  `--files` exit 2. Their printed unit is not a match, so a cap on the match
  set would silently do nothing.

### Adopt: optional semantic query group, additive, not fused

- **Decision (proposed, v2 candidate, gated):** Add `st query --semantic "..."`
  backed by a small local model. The Potion family is the right shape,
  because Model2Vec models are static embeddings, which means a table lookup
  and a pooling step rather than a transformer forward pass, so a pure-Rust
  implementation with no ONNX runtime is feasible. Output is a separate
  ranked-passage result type, not fused with the exact-match rg-compatible
  output.
- **Gate:** Do not start this until the paired agent-level benchmark below
  exists. The entire argument for semantic search is that it saves an agent
  tool calls and tokens. That is a measurable claim, and shipping a model
  before measuring it would mean adopting zvec-grep's largest cost on the
  strength of zvec-grep's chart images.
- **What this is not:** Not BM25, not RRF, not a server, not a managed rg.
  The semantic group returns its own ranked output, and the exact-match group
  continues to return rg-compatible lines. Users who don't pass `--semantic`
  see no model download and no behavior change.
- **Open questions deferred:** which catalog entry to ship by default
  (Potion-code for code-only repos, Potion-retrieval for mixed), whether
  the download stays explicit opt-in (default yes, since the core CLI stays
  zero-download), and whether to add an MCP server for the semantic path.

### Adopt: `st watch` as a watcher plus a debounced durable flush

- **Decision (proposed, v2 candidate, gated):** Add `st watch` as an optional
  foreground or backgrounded process that subscribes to filesystem events and
  flushes accumulated edits into a durable delta segment on a debounce. No
  socket, no port, no resident query handle.
- **Change from the first draft of this document:** the earlier version
  proposed a Unix socket that the CLI could attach to for in-process queries.
  Drop that half. The expensive part of a cold `st` invocation is not opening
  the index, it is re-detecting and re-applying drift. Once the flush is
  durable, a fresh process reads the flushed state off disk in milliseconds
  and a socket buys very little for a large amount of IPC surface.
- **Gate:** Depends on durable flush landing first. Without it, `st watch`
  would be a second copy of the same cross-process staleness problem.
- **What this is not:** Not a managed-rg daemon, not an MCP server, not a
  remote endpoint.

### Reject: BM25, vector fusion, RRF

- **Decision:** No lexical ranking, no dense vectors, no fusion in core
  `st`. If the semantic group above ships, it is a separate query mode with
  separate output.
- **Rationale:** Ranked output breaks count parity with rg/grep, which is
  the harness's correctness anchor (SC-004). RRF adds a probabilistic
  layer that the coverage invariant can't validate. The
  semantic-versus-exact split is a real product boundary, and fusing them
  obscures it.

### Reject: closed index format

- **Decision:** Keep SNTX. Do not adopt `.zvec`.
- **Rationale:** A closed format couples syntext to zvec-ai's release
  cadence, makes debugging and forking intractable, and removes the
  option to validate the segment parser against published bytes. The
  format is small enough (~44 MB on a 500 MB corpus) that an open format
  is a near-zero cost.

### Reject: managed ripgrep via daemon

- **Decision:** `st --rg` (if ever added) would shell out to a ripgrep
  binary in the foreground, not run an embedded rg inside a daemon.
- **Rationale:** Managed rg inside the daemon is one of zvec-grep's
  more interesting choices, because it lets the MCP server return rg-style
  results without the agent needing a rg binary, but it duplicates the
  parser inside Node and ships a new rg dialect to maintain. syntext's
  whole pitch is *be* rg for output. The shell-out keeps a single source
  of truth (the installed `rg` binary) and lets the existing `st` regex
  engine handle the indexed path.

### Reject (deferred): structured prose extractors

- **Decision (deferred):** No Markdown section extractor in v2.
- **Rationale:** zvec-grep's Markdown extractor splits on section headings,
  which is useful for the semantic question ("what does the README say about
  X?"). It is also the only prose-structure extractor they have. JSON, YAML,
  and CSV are chunked as plain 3600-character text, and there is no
  front-matter parsing anywhere in the tree, so the ceiling on this idea is
  lower than it first looks. syntext's answer to that question today is "use
  rg". The `symbols` feature flag already does structure-aware extraction for
  code, and reusing that harness for non-code is a v3 candidate.

## Deferred items

Assessed during this comparison, not implemented:

- **Paired agent-level benchmark.** Run the same task set twice with the same
  agent, model, and prompt, varying only whether `st` is available, and count
  tool calls and input tokens. This is the measurement zvec-grep does and
  syntext does not, and it gates the semantic work above. It is also the only
  way to check whether the rewritten guidance actually reduces tool calls or
  just reads better.
- **Enclosing-symbol field on each hit.** zvec-grep returns the function or
  class containing every hit. The `symbols` feature already extracts symbols
  with tree-sitter, and `ExtractedSymbol` already carries `end_line`, so the
  containment lookup is available. The open work is the output-format
  question, since adding a field to the default line output would break rg
  parity. A `--json` field is the likely shape.
- **`--max-filesize` as a query-time filter.** zvec-grep applies type-aware
  size caps at index time. syntext has size handling at ingestion but no
  query-time equivalent.
- **Two-file storage for large indexes** (`docs/ARCHITECTURE.md` *Potential
  improvements inspired by Cursor*): separating dictionary (always hot,
  small) from postings (large, accessed sparsely) reduces resident memory
  for multi-GB indexes. Requires a new segment format version. Deferred
  until a corpus that actually needs it shows up in the wild.
- **Larger weight-table training corpus** (`docs/ARCHITECTURE.md`):
  syntext trains on ~175 MB, Cursor trains on terabytes, and zvec-grep
  doesn't have a comparable artifact because BM25 doesn't need one. The
  training pipeline (`scripts/weights_gen.py`) already exists, so bumping
  the corpus is a data change, not a code change. Re-evaluate when
  selectivity regressions appear on Java or C# corpora.
- **Semantic catalog pinning policy** (zvec-grep): if the semantic group
  ships, the model revision must be pinned the same way zvec-grep pins it.
  Implementation: hash-locked download, refusal to query if the on-disk
  model doesn't match the pinned revision.

## Prior art cross-reference

- [Google Code Search (Russ Cox, 2012)](https://swtch.com/~rsc/regexp/regexp4.html):
  trigram index plus regex verification. Both syntext and Cursor descend
  from this. zvec-grep does not.
- [Zoekt](https://github.com/sourcegraph/zoekt): trigram index with
  single-file segments. Same lineage as syntext, different segment
  encoding.
- [GitHub Blackbird](https://github.blog/engineering/architecture-optimization/how-we-built-github-code-search/):
  sparse n-grams with frequency-weighted boundaries. Same lineage as
  syntext and Cursor.
- [Cursor fast regex search (2025)](https://cursor.com/blog/fast-regex-search):
  sparse n-grams, CRC32-weighted boundaries, two-file storage. Same
  lineage as syntext. The 6-axis comparison is in `docs/ARCHITECTURE.md`.
- [fff v0.9.4](https://github.com/dmtrKovalenko/fff/tree/v0.9.4):
  resident in-memory bigram prefilter with frecency ranking, MCP server
  integration. See [`COMPARISON_FFF.md`](COMPARISON_FFF.md).
- [zvec-grep v0.2.1](https://github.com/zvec-ai/zvec-grep/tree/v0.2.1):
  BM25 plus dense vector plus RRF plus managed rg plus MCP server. This
  document.

The lineages split cleanly. syntext, Cursor, Blackbird, and Code Search are a
*prefilter* family (n-gram candidate narrowing, then verification), and
zvec-grep is an *IR* family (lexical plus vector ranking, fused via RRF).
Both are valid answers to "agent calls grep too much". They optimize
different parts of the answer.
