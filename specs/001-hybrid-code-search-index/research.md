# Research: Hybrid Code Search Index

**Date**: 2026-03-25 (revised after design review)
**Branch**: `001-hybrid-code-search-index`

---

## 1. Sparse N-Gram Tokenization

**Decision**: Use sparse n-grams with a frequency-trained weight table as the primary content tokenizer. Fall back to trigram behavior naturally (sparse grams bottom out at length 3). Start with CRC32-C weights as the zero-config baseline, then train a byte-pair frequency table from a code corpus.

**Rationale**: Fewer query-time posting list lookups is the single largest performance lever. Sparse n-grams directly minimize this. The algorithm is deterministic, stateless, and parallelizable. The weight table is a fixed 256x256 `u16` array (128 KB), trivial to embed. ClickHouse's O(n) algorithm proves tractability at scale.

**Alternatives Considered**:
- Plain trigrams (Zoekt/Russ Cox): simpler, good for <1M LOC repos, but posting list explosion for common grams. Rejected as primary, but sparse grams degenerate to trigrams as base case.
- Trigrams + bloom/position masks (Cursor phrase-aware): adds 2 bytes per posting entry. Bloom masks saturate after ~8-12 distinct followers for next-char (8-bit bloom) and ~12 occurrences for position mask (8 bits, birthday problem). Useful only in a narrow selectivity band where the gram is uncommon enough not to saturate but common enough that the posting list alone is not selective. That band is thin. **Deferred indefinitely.** Sparse n-grams provide better selectivity without the saturation problem.
- Fixed 4-grams/5-grams: key space explodes (256^4 = 4B keys). Index size prohibitive.
- Positional trigrams: eliminates adjacency false positives but inflates index 4-8x.

**Implementation**:
- **Lowercase normalization**: all input bytes are lowercased via `to_ascii_lowercase()` before gram extraction, both at index time and query time. This enables case-insensitive search without query-time case expansion. See section 15 for tradeoff analysis.
- Index time (`build_all`): extract all substrings where boundary bigram weights exceed interior weights, recursively down to trigrams. Input is lowercased first.
- Query time (`build_covering`): greedy left-to-right, select longest qualifying sparse gram at each position. O(query_length). Pattern is lowercased first.
- Weight function: `weights[byte_a << 8 | byte_b]` from a `[u16; 65536]` table, pre-trained on a code corpus. Weights are computed on lowercased bytes.

**Weight table: pre-trained, not CRC32-C.**

CRC32-C is pseudo-random and has no concept of which character pairs are common in source code. It puts gram boundaries in the middle of `return`, `function`, `import`, producing short, common, non-selective grams. A frequency-trained table is strictly better and costs nothing at runtime (same 128KB const array, same lookup).

Training procedure (one-time, output committed as `src/tokenizer/weights.rs`):
1. Download ~1GB of mixed-language open-source code (Rust, Python, TypeScript, Go, Java, C).
2. Lowercase all content.
3. Count all 65,536 byte-pair frequencies in a single pass.
4. Assign weight = `u16::MAX - normalized_frequency`. Rare pairs get high weight (become boundaries), common pairs get low weight (absorbed into longer grams).
5. Serialize as `pub const WEIGHTS: [u16; 65536] = [...]` in `weights.rs`.

Result: variable names and function names (with rare characters like `Q`, `X`, `_` in unusual positions) produce natural boundaries and long selective grams. Common patterns like `ret`, `for`, `the` get absorbed into longer grams with shorter posting lists.

Estimated effort: 4 hours. Impact: substantially better gram selectivity across all queries.

---

## 2. Segment-Based Index Layout

**Decision**: Single file per segment (Zoekt-style), with a TOC footer recording section offsets. Dictionary section is a flat sorted array of `(u64 gram_hash, u64 postings_offset)` entries, page-aligned for mmap. Postings stored sequentially after the dictionary.

**Rationale**: Minimizes file count, simplifies atomic replacement (rename one file). Zoekt proves this works at scale. A flat sorted array with binary search is simpler and just as fast as FST for fixed-width gram hashes.

**Alternatives Considered**:
- Tantivy multi-file segments: more flexible but overkill for gram+postings-only.
- Cursor two-file (dictionary separate from postings): better memory isolation but adds complexity. Adopt later if needed.
- FST-based dictionary: better for natural-language terms, unnecessary for fixed-width hashes.

**Key decisions**:
- **Manifest**: `manifest.json` with atomic write-then-rename. Contains segment list, git commit SHA, creation timestamp, schema version, opstamp.
- **Overlay**: Generation-based batch commit model (see section 7).
- **Merge**: Background log-merge, trigger at >16 overlay segments, cap merge size at 64MB.

---

## 3. Posting List Operations

**Decision**: Two-tier system. Small/medium lists (<8K entries): delta-varint encoded. Large lists (>=8K): Roaring bitmaps. No probabilistic masks in v1.

**Rationale**: Delta-varint is compact (1-2 bytes/entry) and trivial to implement. Roaring automatically adapts to mixed density and enables SIMD-accelerated Boolean ops for very common grams.

**Why no phrase masks**: The 8-bit position mask saturates after ~12 occurrences of a gram in a file (birthday problem on 8 bit positions). The 8-bit next-char bloom saturates after ~6-8 distinct following characters. For any non-trivial file, common grams will have both masks fully saturated, providing zero filtering exactly where filtering matters most. The engineering cost (mask update logic, saturation monitoring, per-entry overhead) is not justified by the narrow band of usefulness. Sparse n-grams provide better selectivity by design.

**Key algorithms**:
- **Intersection**: Adaptive strategy. Linear merge for similar-sized lists, galloping search when size ratio >32:1.
- **Union (OR queries)**: k-way merge via min-heap with deduplication.
- **Query planning**: Sort posting lists by ascending cardinality (smallest first), skip grams >256x the smallest list. Early termination when intermediate result is empty.

**Crates**: `integer-encoding` for varint, `roaring` (0.11+) for bitmaps.

---

## 4. Regex Decomposition

**Decision**: Build a custom HIR walker using `regex_syntax::parse()` that produces a boolean query tree (`And`/`Or` of gram sets), following Google codesearch's `analyze()` algorithm.

**Rationale**: The built-in `regex-syntax::hir::literal::Extractor` only extracts prefix/suffix literals, not inner literals. For an n-gram index, you need AND/OR composition from all parts of the pattern. The module docs explicitly suggest writing a custom extractor. Google codesearch's algorithm has been in production for over a decade.

**Query tree**:
```
GramQuery = And(Vec<GramQuery>)
          | Or(Vec<GramQuery>)
          | Grams(Vec<u64>)   // gram hashes from a literal, implicitly AND
          | All               // fallback to full scan
          | None              // matches nothing
```

**HIR mapping**:
| HIR Node | Query |
|---|---|
| `Literal(bytes)` | `Grams(sparse_grams_of(bytes))` |
| `Concat(subs)` | `And(analyze(each sub))` |
| `Alternation(subs)` | `Or(analyze(each sub))`, but `All` if any branch is `All` |
| `Repetition(min>=1)` | `analyze(sub)` |
| `Repetition(min=0)` | `All` |
| `Class`, `Look`, `Empty` | `All` |

**Alternatives Considered**:
- Use `Extractor` directly for prefix+suffix AND: misses inner literals (e.g., `a.*foo.*b` gets only `a` and `b`).
- Port Zoekt positional distance checking: good optimization, defer until profiling shows need.
- Ripgrep's `InnerLiterals` + Aho-Corasick: solves single-file scanning, not index querying.

---

## 5. Dependency Audit

**Audit result**: `cargo audit` against RustSec advisory-db: **0 vulnerabilities** in current versions of all target crates.

| Crate | Version | License | Trans. Deps | Recommendation |
|---|---|---|---|---|
| regex | 1.12.3 | MIT/Apache-2.0 | 9 | ADOPT. rust-lang maintained. |
| regex-syntax | 0.8.10 | MIT/Apache-2.0 | (part of regex) | ADOPT. |
| memmap2 | 0.9.10 | MIT/Apache-2.0 | 2 | ADOPT. Treat mapped regions as untrusted. |
| tree-sitter | 0.26.7 | MIT | 25 | ADOPT. Contains C code via cc build. |
| ignore | 0.4.25 | Unlicense/MIT | 22 | ADOPT. ripgrep ecosystem, proven. |
| clap | 4.6.0 | MIT/Apache-2.0 | 23 | ADOPT (with derive). |
| criterion | 0.8.2 | Apache-2.0/MIT | 74 | ADOPT (dev-only). |
| roaring | 0.11.3 | MIT/Apache-2.0 | 3 | ADOPT. Lightweight. |
| rayon | 1.11.0 | MIT/Apache-2.0 | 8 | ADOPT. Parallel index building. |
| crossbeam | 0.8.4 | MIT/Apache-2.0 | 11 | SKIP. rayon uses crossbeam-deque internally. |
| zerocopy | 0.8.47 | BSD-2/Apache-2.0/MIT | 10 | ADOPT. Pairs with memmap2 for zero-copy segment reads. |
| byteorder | 1.5.0 | Unlicense/MIT | 1 | SKIP. zerocopy subsumes its use case. |
| memchr | (transitive via regex) | MIT/Unlicense | 0 | USE. Already present. Use `memchr::memmem` for literal verification fast path. |

**N-gram index crates**: No suitable Rust crate exists. Build from scratch.

**Evaluated but not adopted for v1**:
- `fm-index` (Rust FM-index): see section 8 for analysis. Not production-ready, deferred.
- `pcre2`: optional PCRE2 backend for look-around/backrefs. Deferred until user demand.

**Security notes**:
- memmap2: inherently unsafe (external file modification). Wrap with read-only maps, validate after read.
- tree-sitter: C code compiled via `cc`. Build-time attack surface, not runtime.
- All historical advisories (crossbeam, regex) are fixed in current versions.

---

## 6. Index Storage Budget

**This section addresses the critique that index size estimates were missing.**

### Estimates for a 2GB source corpus (~100K files)

The distribution of posting list sizes is heavy-tailed. Most sparse grams are selective (appear in <1% of files), but the ~5% of grams that degenerate to common short trigrams (3-char grams at boundaries like `the`, `ret`, `for`) appear in 10-60% of files. These dominate storage.

| Component | Size estimate | Ratio to corpus |
|---|---|---|
| Sparse n-gram dictionary | ~8-15MB (400K-700K entries x 20 bytes) | 0.004-0.008x |
| Posting lists, selective grams (95% of grams, avg 200 files) | ~115MB | 0.06x |
| Posting lists, common grams (5% of grams, avg 20K files) | ~600-800MB | 0.3-0.4x |
| Path index (strings + component bitmaps) | ~10-20MB | 0.005-0.01x |
| Document metadata table | ~5-10MB | 0.003-0.005x |
| Symbol index (SQLite, Tier 1 languages) | ~50-100MB | 0.025-0.05x |
| **Total** | **~790MB-1.06GB** | **0.4-0.53x** |

**Hard budget: index must be <= 1x corpus size.** Realistic range is 0.3-0.5x.

**Post-build assertion**: the index builder must log actual index size and warn if it exceeds 0.5x corpus size. This catches poorly-tuned weight tables early.

### Why the original critique's 10-30GB estimate was still wrong

Dense trigrams would be 10-30GB for 2GB of source. Sparse n-grams are 10-30x smaller because (a) 3-5x fewer grams per document, (b) each gram is more selective due to frequency-weighted boundaries, (c) delta-varint compression (1-2 bytes vs 4 bytes per entry). But our initial estimate of 0.15-0.3x was too optimistic because it underweighted the heavy tail of common short grams. The corrected 0.3-0.5x reflects the bimodal distribution.

### Comparison to FM-index alternative

An FM-index at ~1.2GB (0.6x corpus) is comparable to our upper-bound estimate. Neither has a decisive space advantage, which reinforces the decision to choose based on other factors (build speed, incrementality).

---

## 7. Overlay Consistency Model

**This section addresses the critique that the overlay model was underspecified for multi-file atomic edits.**

### Problem

An agent renames a function across 15 files. If overlay updates are per-file, there is a window where a search sees the new name in some files and the old name in others.

### Decision: generational durability with single merged query view

The key insight from the second review: with N overlay generations, each query must do N separate gram lookups and intersections. At 16 overlays, that is 17 segment lookups per query, adding ~8ms of mechanical overhead. This is unacceptable.

**Solution**: separate durability (on-disk generations) from query execution (single merged view in memory).

```
IndexState {
    snapshot: ArcSwap<IndexSnapshot>,  // Atomically swappable, readers get stable view
    pending: Mutex<Vec<FileEdit>>,     // Buffered, invisible to queries
    on_disk_gens: Vec<PathBuf>,        // For crash recovery
}

IndexSnapshot {
    base_segments: Vec<MmapSegment>,   // Immutable base (mmap'd)
    merged_overlay: OverlayView,       // Single logical overlay
    path_index: PathIndex,
    delete_set: RoaringBitmap,         // Union of all deletes from all generations
}

OverlayView {
    gram_index: HashMap<u64, Vec<u32>>,  // Merged gram -> overlay doc_ids
    docs: Vec<OverlayDoc>,              // All dirty files, current content
    next_doc_id: u32,                    // Next overlay-space doc_id
}
```

**API**:
- `notify_change(path)`: buffers the edit in `pending`. NOT visible to queries yet.
- `commit_batch()`:
  1. Write new generation to disk (crash safety).
  2. Rebuild the merged `OverlayView` from ALL dirty files (not just the new batch).
  3. Atomically swap `ArcSwap<IndexSnapshot>`. In-flight searches keep their old snapshot.
  4. After return, all edits are visible.
- `notify_change_immediate(path)`: convenience: `notify_change` + `commit_batch`.

Rebuilding the merged overlay from all dirty files (typically <100) costs ~5-20ms. This is cheaper than managing multiple overlay generations at query time and simpler to reason about.

**Query execution**: always two lookups (base + single merged overlay), not N+1.
1. Get candidates from base segments.
2. Subtract `delete_set`.
3. Get candidates from `merged_overlay.gram_index`.
4. Union results.
5. Verify against current content.

**Concurrent reads/writes**: `ArcSwap<IndexSnapshot>` ensures searches get a stable snapshot. `commit_batch()` creates a new `IndexSnapshot` and atomically swaps the pointer. In-flight searches on the old snapshot are unaffected. This is the standard Tantivy pattern.

**On-disk generation cleanup**: background thread collapses old on-disk generations into a single overlay segment file after each `commit_batch()`. This bounds disk usage without affecting query speed.

**Full reindex trigger**: merged overlay covers >30% of base files, or `git checkout`/branch switch changes >50% of files. Full reindex is the only mechanism that cleans stale doc_ids from base posting lists. Overlay compaction does not rewrite the base.

---

## 8. FM-Index Evaluation

**This section addresses the critique that FM-indexes were "completely ignored."**

### What FM-indexes offer

An FM-index (BWT + wavelet tree + sampled suffix array) provides:
- O(m) count queries for exact substrings (m = pattern length)
- O(m + k*s) locate queries (k = occurrence count, s = SA sampling rate)
- Space: 0.5-1.5x corpus size depending on SA sampling

### Why FM-index is NOT the right choice for v1

**1. Construction time is prohibitive for the P1 "first-run experience" goal.**
SA-IS suffix array construction on 2GB is 60-120s on a modern laptop. Add BWT, wavelet tree, and SA sampling: total 90-180s. Our sparse n-gram index builds in 5-20s for the same corpus. The spec requires sub-5s for typical repos (up to 500K LOC). A 2GB monorepo is a stretch target, not typical, but even there 90-180s vs 5-20s is a 10x difference.

**2. Locate is expensive for high-frequency patterns.**
`count()` is O(m) and fast. But `locate()` with SA sampling rate 64 requires 64 LF-mapping steps per occurrence, each needing a wavelet tree rank query. Locating 1000 occurrences of a common identifier: ~64K LF-mapping steps. Realistic cost: 10-50ms, not the "sub-1ms" the critique implies.

**3. Zero incrementality.**
FM-indexes cannot be updated in place. The critique's workaround ("scan modified files directly") is fine for 50 files but breaks down at 500+. The n-gram overlay model handles arbitrary edit volumes gracefully.

**4. The Rust `fm-index` crate is not production-ready.**
Small project, limited optimization, no SIMD wavelet tree, no mmap support. Building a production-quality FM-index from scratch is a multi-month effort.

**5. Marginal benefit over well-tuned sparse n-grams for literals.**
A literal "process_batch" decomposes into 3-5 covering sparse grams, each highly selective. Intersection yields a small candidate set. Verification with `memchr::memmem` on candidates is sub-millisecond. Total: 2-5ms. FM-index: 0.5-2ms. The 1-3ms difference does not justify the architectural complexity.

### When FM-index becomes worthwhile (v2+)

- If profiling shows >50% of query time is spent on literal queries AND the sparse n-gram false positive rate exceeds 5% for those queries.
- If a production-quality Rust FM-index crate emerges.
- As a parallel engine (not replacement): FM-index for literals, n-gram for regex, with a query router selecting the engine.

**Decision**: defer FM-index to v2. Note it as a valid optimization path. Design the query router (section 9) so a second engine can be plugged in later without architectural changes.

---

## 9. Query Router

**This section addresses the critique that there was no query planner.**

The critique proposes a full cost-based optimizer with cardinality estimation. This is overengineered for v1. What we need is a query router that selects the fast path.

### Decision: three-tier query routing

```
Input pattern
  |
  +-- Is it a fixed literal (no regex metacharacters)?
  |     YES --> Literal fast path:
  |               1. Extract sparse covering grams
  |               2. Intersect posting lists (smallest-first)
  |               3. Verify with memchr::memmem (not regex)
  |
  +-- Does regex decomposition yield extractable grams?
  |     YES --> Indexed regex path:
  |               1. Build GramQuery tree from HIR
  |               2. Execute posting list operations
  |               3. Verify with compiled regex
  |
  +-- No extractable grams (e.g., `.*`, `[a-z]+`)
        --> Full scan path:
              1. Apply path filter if present (cheap)
              2. Scan all matching files with compiled regex
```

**Path filter always executes first when present.** A `path:src/api/*.rs` filter reduces the file set before any content index lookup. Path filtering is a Roaring bitmap AND, essentially free.

**Literal detection**: check if `regex_syntax::parse(pattern)` produces a single `Literal` HIR node, or use a simpler heuristic: pattern contains no regex metacharacters (`\.[{()*+?|^$`). The simpler heuristic is sufficient because the consequence of misclassification is only a minor performance difference (regex compile + verify vs. memchr), not a correctness issue.

**The query router is explicitly designed to be extensible.** A v2 FM-index engine or symbol index can be added as new paths without changing the existing three.

### Why not a full cost-based optimizer

- The posting list cardinality-based ordering (smallest first) already handles the most important optimization.
- For v1, there are only two content index types (gram postings and path index). A cost model over two index types is not worth the complexity.
- When a third engine (FM-index, symbol index) is added in v2, a cost model becomes justified.

---

## 10. Path Index Design

**This section addresses the critique that path indexing was underspecified.**

### Decision: component-based Roaring bitmap index

```
PathIndex {
    paths: Vec<String>,                         // sorted by path, file_id = position
    extension_to_files: HashMap<String, RoaringBitmap>,  // ".rs" -> {file_ids}
    component_to_files: HashMap<String, RoaringBitmap>,  // "api" -> {file_ids}
}
```

**Why Roaring bitmaps**: path component sets are often dense (e.g., `.rs` matches 40% of files). The universe is contiguous file_ids. Roaring excels here.

**Query integration**: path filter produces a file_id bitmap. All subsequent posting list intersections are AND'd with this bitmap. Cost: single Roaring AND, negligible.

**Language classification**: extension-based mapping (`rs` -> Rust, `py` -> Python, etc.). Used for symbol index tier selection and `-t`/`-T` CLI flags. Stored in the path index, not a separate structure.

---

## 11. Symbol Index Design

**This section addresses the critique that the symbol index was "hand-wavy."**

### Decision: separate command/mode in v1, planner integration in v2

Symbol search is a distinct operation, not integrated into the text search pipeline for v1. The API exposes `search_symbols(name, kind, language)` as a separate method. The CLI exposes it as a query prefix (`sym:process_batch`) or separate subcommand.

### Storage

SQLite (WAL mode) for the symbol index. Justified because:
- The symbol index is small (50-100MB for a 2GB repo).
- Read-heavy, write-light (rebuilt on index build, incremental via Tree-sitter re-parse).
- SQLite's WAL mode provides good concurrent read performance.
- Complex queries (name LIKE, kind filter, language filter) map naturally to SQL.

### Schema

```sql
CREATE TABLE symbols (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,      -- 'function', 'struct', 'class', 'trait', 'method', etc.
    file_id INTEGER NOT NULL,
    line INTEGER NOT NULL,
    column INTEGER NOT NULL,
    span_end_line INTEGER,
    span_end_column INTEGER,
    language TEXT NOT NULL
);
CREATE INDEX idx_name ON symbols(name);
CREATE INDEX idx_kind_name ON symbols(kind, name);
CREATE INDEX idx_file ON symbols(file_id);
```

### Language tiers

- **Tier 1 (always indexed if tree-sitter grammar available)**: Rust, Python, TypeScript/JavaScript, Go, Java, C/C++.
- **Tier 2 (indexed if grammar installed)**: Ruby, Swift, Kotlin, etc.
- **Tier 3 (heuristic fallback)**: regex-based definition extraction (`^\s*(def|fn|func|function|class|struct|enum|trait|interface)\s+(\w+)`) for unsupported languages.

### Why not integrate into the query planner for v1

The text search pipeline (gram decomposition -> posting intersection -> verification) and the symbol search pipeline (SQLite query -> file lookup) have no shared execution path. "Combining" them means running both and intersecting results, which is trivial to do in application code. A formal planner that chooses between them based on cost estimation is not justified until there are 3+ index types to choose from.

---

## 12. Benchmark Targets (Revised)

**This section addresses the critique that "sub-50ms" is too vague.**

### Primary metric: selectivity

**Target**: candidate set passed to the regex verifier must be < 0.5% of total files for >= 95% of real queries. This is the metric that determines whether the index is useful.

### Decomposed latency budget (warm cache, 2GB corpus)

| Phase | Target | Measurement |
|---|---|---|
| Query parsing + gram extraction | < 0.5ms | Microbenchmark |
| Dictionary lookups (binary search, mmap) | < 0.1ms per gram | Microbenchmark |
| Posting list reads from mmap | < 1ms total | Microbenchmark at various list sizes |
| Posting list intersection (3-5 grams) | < 2ms | Microbenchmark with realistic cardinalities |
| Path filter (Roaring bitmap AND) | < 0.1ms | Microbenchmark |
| Verification (regex/literal on candidates) | < 5ms for < 0.5% candidate set | Benchmark at 0.1%, 0.5%, 1%, 5% selectivity |
| **End-to-end warm query** | **< 10ms p50, < 20ms p95** | Integration benchmark |

### Other targets

| Metric | Target |
|---|---|
| Cold query p95 (first query after index load) | < 100ms |
| Full index build (500K LOC repo) | < 5s |
| Full index build (2GB monorepo) | < 30s |
| Incremental overlay commit (15-file batch) | < 50ms |
| Memory resident (dictionary + metadata, not postings) | < 100MB for 2GB corpus |
| Index size on disk | < 1x corpus size |

### Comparison baselines

- **ripgrep (no index)**: 200ms-15s depending on repo size.
- **Zoekt**: 10-50ms warm queries (trigram index).
- We target **2-4x faster than Zoekt** for warm literal queries via sparse n-gram selectivity.

---

## 13. Content-Defined Chunking Evaluation

**This section evaluates the critique's proposal to use 4KB chunks instead of files as the document unit.**

### What chunking offers

- Better selectivity for large files (a match in a 50K-line file only triggers verification of the ~4KB chunk, not the entire file).
- Cheaper incremental updates (re-index only changed chunks).
- Bounded verification cost per candidate.

### Why it is premature for v1

1. **Most code files are small.** In a typical repo, median file size is 5-20KB. Chunking these at 4KB produces 1-5 chunks per file. The selectivity improvement for a 10KB file going from "file-level" to "chunk-level" is at most 2.5x, often less.

2. **Posting list inflation.** 100K files chunked at 4KB average = 500K chunks. Every gram's posting list grows ~5x in entry count. The index size budget depends on posting list sizes, so this partially negates the "smaller" claim.

3. **Boundary correctness is tricky.** A search pattern straddling a chunk boundary requires chunk overlap (64 bytes at each boundary). This adds ~1.6% to indexed content but introduces complexity in deduplication and offset reporting.

4. **The real bottleneck is selectivity, not verification speed.** If the index narrows to 50 candidate files averaging 20KB, verification scans ~1MB. With `memchr::memmem` at 10+ GB/s, that is under 0.1ms. Chunking would reduce this to ~0.02ms. The difference is noise.

5. **Where chunking actually matters**: files > 1MB (generated code, minified JS, data files). These should be handled by a size limit (skip or split), not by a global chunking strategy.

### Decision

File-level documents for v1. Add content-defined chunking as a v2 optimization, triggered by profiling data showing large files dominate verification cost. Design the segment format so chunk_ids can replace file_ids without a format break (use u32 IDs that can represent either).

---

## 14. Architecture Summary (Revised)

### Query Pipeline

```
User Pattern
    |
    v
[Query Router]
    |
    +-- Literal? --> [Sparse Gram Extraction]
    |                      |
    +-- Regex?  --> [HIR Walker / Gram Decomposition]
    |                      |
    +-- No grams? -------> [Full Scan] ----+
    |                      |               |
    |                      v               |
    |               [Path Filter]          |
    |               (Roaring AND)          |
    |                      |               |
    |                      v               |
    |               [Query Planner]        |
    |               (sort by cardinality)  |
    |                      |               |
    |                      v               |
    |               [Posting Intersection] |
    |               (base + overlays)      |
    |                      |               |
    |                      v               |
    |               [Candidate File IDs]   |
    |                      |               |
    +----------------------+---------------+
                           |
                           v
                    [Verification]
                    (memchr for literals,
                     regex crate for patterns)
                           |
                           v
                       [Results]
```

### Index Structure (per segment file)

```
+---------------------------+
| Header (40 bytes)         |  magic, version, counts, offsets
+---------------------------+
| Document Table            |  doc_id -> (path, content_hash, size)
+---------------------------+
| Postings Section          |  sequential posting lists, delta-varint or roaring
+---------------------------+
| Dictionary Section        |  sorted (gram_hash, offset, count), page-aligned
+---------------------------+
| TOC Footer (48 bytes)     |  offsets, checksum, magic
+---------------------------+
```

### Update Flow

```
Git HEAD commit (base) --> Immutable segments (mmap'd)
                              |
Agent edits --> pending_edits buffer (invisible to queries)
                              |
commit_batch() --> Write generation to disk (durability)
                   Rebuild merged OverlayView (all dirty files)
                   ArcSwap snapshot (atomic visibility)
                              |
                         Query reads: base - delete_set + merged_overlay
                              |
                         Full reindex when overlay > 30% of base
```

### Storage Layout

```
.ripline/
├── manifest.json              # segment list, base commit, overlay gen
├── segments/
│   ├── {uuid}.seg             # immutable base content index segments
│   └── ...
├── overlay.seg                # consolidated on-disk overlay (crash recovery)
├── overlay.del                # deletion bitmap (Roaring serialized)
├── paths/
│   ├── paths.dat              # sorted path strings
│   └── components.dat         # component -> Roaring bitmap
└── symbols/
    └── symbols.sqlite         # symbol index (WAL mode)
```

Note: in-memory `OverlayView` (rebuilt on each `commit_batch()`) is authoritative during operation. On-disk overlay files are only used for crash recovery and startup.

---

## 15. Case-Insensitive Search

**This section addresses a cross-cutting gap: how case-insensitivity interacts with gram extraction.**

### Problem

If the user searches for `ParseQuery` with `-i`, the pattern matches `parsequery`, `PARSEQUERY`, `parseQuery`, etc. But grams from `ParseQuery` (e.g., the sparse gram `ParseQ`) exist only in the posting list for those exact bytes. The posting list for `parseq` is a different entry.

### Options evaluated

1. **Lowercase normalization at index time**: index stores grams from lowercased file content. Query grams are also lowercased. The verifier runs against original content for correct output. Tradeoff: posting lists for `par` and `Par` merge, reducing selectivity for case-sensitive queries.

2. **Case expansion at query time**: for each gram, enumerate all case variants. A 3-char gram has 2^3 = 8 variants; a 5-char gram has 32. Union all variant posting lists. Combinatorially ugly and slow for longer grams.

3. **Full scan fallback for case-insensitive queries**: simple but defeats the index.

4. **Dual index (original + lowercase)**: two sets of posting lists. Doubles index size.

### Decision: lowercase normalization at index time

Source code is ~85% lowercase by character frequency. Merging `Par`/`par`/`PAR` into one posting list increases list sizes by roughly 15-20%, not 2x. The selectivity loss is bounded and acceptable.

**Implementation**:
- Tokenizer normalizes input bytes to lowercase via `to_ascii_lowercase()` before computing gram hashes. Both at index time and query time.
- The segment stores grams from lowercased content. There is no "case-sensitive gram mode."
- Case-sensitive queries still work: the index returns a superset of candidates (because lowercase grams match more broadly), and the verifier (which runs against original file content) filters out case mismatches. This increases verification work slightly but never produces wrong results.
- Case-insensitive queries: the verifier uses `regex::RegexBuilder::new(pattern).case_insensitive(true)` or `memchr::memmem::FinderBuilder::new().build(lowercase_pattern)`.

**Tradeoff documentation**: for case-sensitive searches on patterns where case matters (e.g., `ParseQuery` vs `parsequery`), the index may return ~15-20% more candidates than a case-aware index would. Verification eliminates these. If profiling shows this is a problem, add a case-sensitive gram dictionary as a v2 optimization (dual index).

---

## 16. Gitignore Handling in Overlays

### Problem

When an agent creates a file, `notify_change()` is called. If that file is in `.gitignore` (build artifact, generated file), should the overlay index it?

### Decision

`notify_change()` checks `.gitignore` rules and silently skips ignored files. The `ignore` crate supports standalone gitignore matching via `ignore::gitignore::Gitignore::new()` followed by `matched()`. This is O(1) per call (pattern matching against the compiled gitignore rules).

**Behavior**:
- On full `build()`: the `ignore` crate's directory walker already respects `.gitignore`. No change needed.
- On `notify_change(path)`: check `gitignore.matched(path, is_dir=false)`. If ignored, return `Ok(())` without buffering.
- The caller (agent/editor) does not need to pre-filter. The index handles it.
- If `.gitignore` changes during a session, the `Gitignore` matcher should be reloaded on the next `commit_batch()` or `build()`.

---

## 17. Build Parallelism Strategy

### Problem

Full index build on a 2GB corpus produces ~2.5B (gram, doc_id) pairs. At 12 bytes each (u64 hash + u32 doc_id), that is ~30GB, far too large for an in-memory sort.

### Decision: batched-segment build

Split files into batches of ~256MB of source content. Each batch produces one segment file independently. Segments are tracked in the manifest and queried in parallel at search time. Merge lazily during compaction.

**Pipeline per batch**:
1. Enumerate files for this batch (serial, from the file list).
2. Read file content (parallel via rayon).
3. Sparse gram extraction per file (parallel, embarrassingly parallel).
4. Aggregation: each file produces `Vec<(u64, u32)>` (gram_hash, doc_id). Concatenate all pairs for the batch.
5. Sort by gram_hash (parallel sort via rayon's `par_sort_unstable`).
6. Sequential scan to emit posting lists: walk sorted pairs, emit a posting list each time the gram_hash changes.
7. Write segment file (sequential: postings section, then dictionary, then TOC).

**Memory budget per batch**: ~256MB of source content, ~1GB of (gram_hash, doc_id) pairs (worst case: 4 grams per byte of source), ~256MB for segment output buffer. Peak: ~1.5GB per batch. Acceptable for a laptop.

**Batch count for 2GB corpus**: ~8 batches, producing 8 segments. These are merged lazily by the compaction policy (merge when >10 segments). Or merge immediately after build if desired.

**Why sort-based over map-reduce**: a map-reduce approach (thread-local HashMaps merged at the end) requires O(threads * distinct_grams * sizeof(Vec)) memory. With 8 threads and 500K distinct grams, that is 8 * 500K * 24 bytes (Vec overhead) = ~96MB just for the map structure, plus the actual posting data. The sort-based approach uses a single flat buffer, which is more predictable and cache-friendly.

**Rayon integration**: `rayon::par_iter()` over files for reading and gram extraction. `rayon::par_sort_unstable()` for the (gram_hash, doc_id) pairs. Sequential segment write (I/O-bound, parallelism does not help).

---

## 18. Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Sparse n-gram weight function poorly tuned for specific codebase (generated code, minified JS) | Medium | Medium | Size limit for non-source files. Per-repo weight training as v2 feature. Fallback to dense trigrams for high-entropy content. |
| Posting list sizes for common short grams blow up index size | Medium | Medium | Roaring bitmaps for lists > 8K. Query planner skips grams > 256x smallest. Post-build assertion warns if index > 0.5x corpus. |
| Tree-sitter grammar bugs cause symbol index corruption | High | Low | Symbol index is advisory. Queries degrade gracefully to text search. Wrap parse calls in panic catch. |
| Overlay rebuild cost on large dirty sets (>500 files) | Low | Medium | Full reindex triggers at 30% threshold. Between 100-500 dirty files, overlay rebuild is ~50-200ms, acceptable. |
| Large monorepos (> 5GB) exceed build time budget | Medium | Medium | Batched-segment build (256MB per batch) bounds memory. Consider FM-index for v2. |
| Chunk boundary misses for large files | Medium | Low | v1: skip files > 10MB by default, configurable. v2: content-defined chunking with 64-byte overlap. |
| Case-sensitive query selectivity degraded by lowercase normalization | Medium | Low | ~15-20% more candidates, eliminated by verifier. Dual index (original + lowercase) as v2 optimization if profiling warrants. |
| Stale doc_ids in base segments degrade selectivity over time | Low | Low | Full reindex at 30% overlay threshold is the only base cleanup mechanism. Stale entries are harmless (verifier filters them) but waste posting list space. |
| Concurrent ArcSwap contention under heavy write load | Low | Low | `commit_batch()` holds the pending lock briefly. Snapshot swap is a single atomic pointer write. No contention unless >100 commits/sec. |

---

## 19. Future Work (v2+)

Explicitly scoped out of v1, but architecturally accommodated:

1. **FM-index engine** for literal queries. Plugs into the query router as a fourth path.
2. **Large file verification optimization**: two alternatives evaluated.
   - *Content-defined chunking*: 4KB Rabin-fingerprint chunks as document unit. Better selectivity, but 5x posting list inflation and boundary overlap complexity.
   - *Block-level positional data*: for files >64KB, record which 64KB block contains each gram in the posting entry. Verify only matching blocks. Simpler than chunking, no posting list inflation, bounded verification cost. **Likely the better v2 choice.**
3. **Symbol index planner integration**. Cost-based optimizer over 3+ index types.
4. **SIMD-accelerated posting list intersection** (AVX2/NEON). Only if profiling shows intersection is the bottleneck.
5. **Workload-adaptive gram selection** per recent research (April 2025 evaluation of FREE/BEST/LPMS strategies). Agent query patterns are somewhat predictable; adaptive selection could improve selectivity.
6. **PCRE2 backend** for look-around/backreference support, with per-query timeout.
7. **Learned sparse retrieval** (SPLADE-style) for semantic symbol search.
8. **Dual dictionary (original + lowercase)** for case-sensitive query selectivity if profiling shows the 15-20% overhead matters.
9. **Eytzinger layout for dictionary binary search**. ~2-3x cold lookup improvement, but dictionary lookup is <0.15% of query budget. Only relevant if cold-cache p95 is a problem.
