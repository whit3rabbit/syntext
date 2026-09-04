# Deferred work

Verified-but-not-implemented items and known follow-ups. Each is an issue
candidate; the "why" is the gating concern to resolve before picking it up.
**Do not implement blind** — re-confirm the concern still holds against current
code first.

## Perf-review items (adopt-verified-improvements, 2026-07-10)

A 17-item external perf review was adopted on `perf/adopt-verified-improvements`
(Phase A #9/#10/#4/#3; Phase B #6/#12/#1; Phase C #2/#7/#17; Phase D #5). The
items below were verified but deliberately left out.

### #15 — reuse the verifier's file bytes in `render_json`

`src/cli/render/json.rs`. `render_json` re-reads + re-tokenizes each matched
file that the verifier (`resolve_doc`) already read into an `Arc<[u8]>`.
Attaching that Arc to `SearchMatch` would save the re-read.

**Why deferred:** the resolver's Arc is encoding-normalized
(`normalize_encoding`), but `--json`'s `bytes_searched` stat is computed from the
*raw* file length; reusing the normalized Arc would change that stat for
non-UTF-8 / BOM files, and `--json` is oracle-compared (`oracle_cli`). Gain is
only a warm-page-cache re-read. A correct fix keeps the raw byte length for the
stat while reusing the normalized bytes for line / submatch rendering.

### #11 — streaming intersection with early-exit

`src/search/executor.rs` `execute_query` materializes the full candidate bitmap
→ `Vec<u32>` before the verify loop applies `max_results`.

**Why deferred:** the expensive part (resolve + verify, file I/O + regex) is
*already* early-exited via the atomic `match_count` counter in
`src/search/mod.rs` (`do_match` returns `None` once the limit is hit). #11 would
only avoid materializing the candidate-id Vec (~400 KB / microseconds for 100k
candidates), and streaming sequentially would sacrifice the rayon parallelism
that dominates real latency. It also changes candidate production order → `-m`
semantics, so it must gate hard on `oracle_self` / `oracle_cli`. Lowest reward,
highest risk of the batch.

### #13 — `build_all` transient Vec into HashSet

`src/tokenizer/mod.rs` (callers `src/index/build.rs`, `src/index/delta_apply.rs`).
`build_all` returns a `Vec<u64>` immediately `.into_iter().collect()`-ed into a
`HashSet<u64>` per file.

**Why deferred:** build-time only, and the win is just one transient Vec alloc
per file (the HashSet still hashes every gram either way). Needs `build_all` to
push into a caller-provided sink (a `build_all_into` + generic-sink refactor of
`append_grams_for_boundaries`) or a reused per-thread buffer. Marginal; do only
if index-build profiling flags it.

### #8 — DROPPED (not deferred)

`src/cli/render/flat.rs`. The claimed "3× `find_iter` per line" only occurs in
the rare `--replace --column --max-columns` combo, and those passes compute
genuinely different things (replaced text, spans over the *replaced* line,
match-count over the *original* for the omitted-line placeholder), so they are
not redundant. The common color path is already one `find_iter` / line. No
action.

### #14 — SKIPPED

`calibrate_threshold` reads sample files twice. Build-time only; the review
itself said skip unless profiling shows it.

## Cross-cutting follow-ups

### Cross-struct path interner (from #17)

Interning (`70aee57`) covers only the three path copies *inside* `PathIndex`.
`BaseSegments.base_doc_paths` and `path_doc_ids` keys (`src/index/snapshot.rs`)
hold two further owned copies of the same path set. Sharing those needs a
cross-struct interner + a wide `Arc<Path>` type ripple through
search / resolver / json / stats. The `open_search_e2e` 100k nightly bench
remains the RSS gate.

## From the zvec-grep comparison (2026-09-04)

Assessed against [zvec-grep v0.2.1](https://github.com/zvec-ai/zvec-grep/tree/v0.2.1)
and deliberately not implemented. Full reasoning, including what was adopted
and what was rejected outright, is in [`COMPARISON_ZVEC.md`](COMPARISON_ZVEC.md).

### Paired agent-level benchmark

Run one task set twice with the same agent, model, and prompt, varying only
whether `st` is available, and count tool calls and input tokens. syntext
measures grep latency; this measures the thing the agent pitch actually claims.

**Why it comes first:** it gates the semantic work below. The whole argument
for semantic search is that it saves an agent tool calls and tokens, which is
a measurable claim. It is also the only way to check whether the rewritten
injected guidance (`src/hook/core/instructions.rs`) reduces tool calls or
merely reads better.

### `--semantic` query group

A separate ranked-passage mode backed by a small local model, not fused with
the exact-match output. Model2Vec models are static embeddings (a table lookup
plus pooling, not a transformer forward pass), so a pure-Rust implementation
with no ONNX runtime is feasible.

**Why deferred:** gated on the benchmark above. Shipping a model download and a
second output shape on the strength of someone else's chart images would be
adopting their largest cost without evidence. Ranked output also breaks the
count parity with rg/grep that is the harness's correctness anchor (SC-004),
which is why it must stay a separate mode rather than a fusion.

### `st watch`

A watcher process that flushes accumulated edits into a durable delta segment
on a debounce. No socket, no port, no resident query handle.

**Why deferred:** its original gate (durable flush) has landed
(`src/index/flush.rs`), which also removed most of the urgency. Drift inside
the per-search budget never needs a watcher, and past that `st update` already
persists. The remaining win is latency: a watcher reacts in under a second, a
search reacts on the next invocation. Do not add the Unix socket the first
draft proposed: the expensive part of a cold `st` run is re-detecting drift,
not opening the index, so a socket buys little for a lot of IPC surface.

### Enclosing-symbol field on each hit

zvec-grep returns the function or class containing every hit. The `symbols`
feature already extracts symbols with tree-sitter and `ExtractedSymbol` already
carries `end_line`, so the containment lookup is available.

**Why deferred:** the open question is the output format, not the lookup.
Adding a field to the default line output breaks rg parity. A `--json` field is
the likely shape, and `--json` is oracle-compared (`oracle_cli`), so it needs
the same "only when explicitly requested" treatment `--max-results` gave
`"truncated"`.

### `--max-filesize` as a query-time filter

Currently accepted and warned about as unimplemented
(`src/cli/args/compat.rs`). zvec-grep applies type-aware size caps at index
time (1 MiB code, 256 MiB text, 16 MiB structured data, 10 MiB images).
syntext has size handling at ingestion but no query-time equivalent.
