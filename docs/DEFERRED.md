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
