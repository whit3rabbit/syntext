# Building a Hybrid Code Search Index in Rust for Agent Workflows

## Context and performance requirements

Agentic coding workflows disproportionately stress one operation: “find me the exact thing” by literal or regular-expression search across a repository, often repeatedly and in parallel. entity["company","Cursor","ai code editor"] explicitly frames this as a return to `grep`-style behavior: modern agents “love to use `grep`,” even though developer tooling has long relied on syntactic/semantic indexes for code navigation. citeturn10view0

The practical bottleneck is not (only) regex execution speed inside a single file; it is repository-wide scanning. Cursor argues that regardless of how fast `ripgrep` can match within a file, a scan-based tool still has to touch *all* files, and they report seeing `rg` invocations taking more than 15 seconds in large monorepos—long enough to “stall” the agent-feedback loop. citeturn10view0 This matches a long-running theme in code search research: indexed candidate selection is what breaks the linear “scan everything” cost curve for regex-like queries, by using an index to narrow to a much smaller set of plausible documents and verifying matches only for that candidate set. citeturn1view0turn4search27turn8view0

At the same time, classic IDE navigation tasks (definition lookup, references, structurally-scoped search) are driven by language-aware symbol information rather than raw text. Cursor explicitly points to the historical arc from tools like `ctags` toward standardized language-server driven functionality (LSP-style capabilities) for “Go To Definition” and similar navigation. citeturn10view0turn5search2 The upshot is that “fast agent search” is not one retrieval problem—it is at least two: (1) text/regex/pattern retrieval across arbitrary files, and (2) code-structure-aware symbol retrieval.

## Why hybrid indexing wins in practice

A “hybrid” design is not a compromise; it is the natural result of the fact that large, production code search systems already split the world into multiple indexes and then combine them at query time.

A concrete example is entity["company","GitHub","code hosting platform"]’s description of its Rust-based code search engine (“Blackbird”). Their query execution pipeline rewrites a user query into multiple iterator clauses, including n-gram iterators for *content*, *paths*, and *symbols* (e.g., `content_grams_iter(...)`, `paths_grams_iter(...)`, `symbols_grams_iter(...)`) and then executes boolean logic as intersections/unions of these iterators, followed by a document-level verification step. citeturn9view0 This is, in effect, a multi-index plan: file/path constraints, content substring constraints, and symbol-derived constraints.

Similarly, Zoekt (the trigram-based engine used widely for code search) explicitly indexes both file contents and filenames, and discusses ranking signals based on symbol definitions, noting that symbol detection often relies on external tooling such as `ctags`. citeturn8view0turn4search2

Taken together, the empirical direction of the field is consistent: the most robust practical architecture is multi-layered—cheap filters first, richer/semantic filters when available, and an exact verifier last. citeturn9view0turn8view0turn1view0

## Fast regex and literal search using n-gram indexes

### The baseline: inverted index + posting-list intersections + verifier

Cursor’s “Fast regex search” post starts from classic inverted-index mechanics: tokenize documents, map tokens to posting lists (document IDs), and answer multi-token queries by loading posting lists and intersecting them. citeturn10view1 This is the foundation of n-gram candidate generation: represent each file as a “document,” index overlapping n-grams as “tokens,” and then select candidate files by intersecting postings for tokens implied by the query. citeturn10view1turn4search27

Cursor also emphasizes a critical correctness point: trigram/n-gram indexing is not (by itself) a regex engine. It is a *prefilter* that yields a superset of potentially matching documents; the final result set still requires matching the regex “the old fashioned way” on the underlying text. citeturn1view0 Zoekt describes the same architecture: extract substrings from regexes to form an indexed query, then validate matches by running the full regex on the candidate documents. citeturn8view0

### Why trigrams are the classic sweet spot, and why they still struggle

Cursor restates the canonical trigram design tradeoff: bigrams create too few keys—posting lists become very large—while quadgrams explode the key space into the billions; trigrams are a workable middle ground. citeturn10view1 entity["company","GitHub","code hosting platform"] makes essentially the same observation in its code search writeup, pointing out that bigrams are not selective enough and quadgrams are too space-intensive, and describing trigrams as a known “sweet spot” that nevertheless becomes problematic at GitHub’s scale. citeturn9view0

Even with trigrams, false positives can be expensive: a document may contain all the trigrams extracted from a pattern but not in the correct adjacency/structure, forcing costly content fetch and verification. GitHub explicitly calls this out as a source of “slow queries” for common grams and highlights adjacency-related false positives as a core issue. citeturn9view0

### Cursor’s “phrase-aware trigram index”: tiny probabilistic masks to reduce false positives

Cursor proposes an augmentation to classic trigram postings that stores (for each trigram + document) two 8-bit masks:
- a **position mask** reflecting trigram start offsets modulo 8, and
- a **“next character” bloom-like mask** hashing the character following each trigram occurrence. citeturn2view0turn3view0

With these two bytes per posting, Cursor claims two benefits:
- The “next character” mask lets the trigram-keyed index behave like it can be queried with quadgram-like specificity (“query it using quadgrams”) while still storing trigram keys. citeturn2view3turn3view0
- The position mask supports a cheap adjacency test: shift/rotate position bits to check whether two trigrams can occur consecutively in the document, reducing cases where trigrams exist but are far apart. citeturn2view3turn3view0

Cursor also notes the probabilistic nature of bloom-like masks (false positives possible, but acceptable because the verifier enforces correctness), and highlights a key operational drawback: small bloom filters can saturate as they are updated, becoming non-selective and making in-place updates painful. citeturn2view0

### Sparse n-grams: shifting cost from query time to index time

Both Cursor and GitHub converge on **variable-length/sparse grams** as a way to reduce the “too many postings lookups / too many false positives” problem while keeping query-time work bounded.

GitHub describes moving to dynamic gram sizes (“sparse grams”), motivated by the fact that common trigrams (like `for`) are not selective enough at their scale, and describes a tokenization approach based on assigning weights to bigrams and selecting intervals where internal weights are strictly smaller than boundary weights, recursively, down to trigrams. At query time, it keeps only the “covering” grams because others are redundant. citeturn9view0

Cursor presents a closely related sparse n-gram idea: instead of extracting every consecutive trigram, assign deterministic “weights” to character pairs and emit substrings where weights at both ends are strictly greater than weights inside; at query time, generate only a minimal covering set of n-grams to reduce posting list lookups. Cursor further suggests an optimization: choose a weight function based on empirical character-pair frequencies from a large code corpus so that rare pairs get high weight, which leads to fewer query-time lookups and fewer candidate documents. citeturn3view0turn3view1

This approach is not limited to code search engines. ClickHouse’s `sparseGrams` work item and documentation describe a similar mechanism—hash bigrams (often with CRC32), then extract substrings where boundary hashes exceed internal hashes—illustrating that sparse-gram tokenization is being productized as a general technique for substring/regex-adjacent filtering. citeturn11view0turn4search13

## Segment-based index architecture for fast reads and practical writes

A high-performance implementation detail matters as much as the abstract indexing idea: how the index is laid out on disk and updated.

### Why “append-only segments” dominate real systems

Lucene-style systems organize indexes as **immutable segments**: as documents are added, new segments are flushed, and updates/deletes create new segments rather than mutating existing ones. citeturn0search21 This design has two direct benefits for speed-first code search:
- reads can be effectively lock-free against concurrent writes (readers can keep using stable segments), and
- writes become sequential flushing + background merging rather than random in-place mutation. citeturn0search21turn0search10

Tantivy (a Rust search engine library inspired by Lucene) uses the same core idea: an index as a collection of smaller independent immutable segments, tracked via metadata. citeturn0search1

This architecture is also explicitly visible in Zoekt: its index is organized into “shards” laid out to be memory-mapped efficiently, and it stores posting lists using varint encoding. citeturn8view0

image_group{"layout":"carousel","aspect_ratio":"16:9","query":["Lucene segment merge diagram","LSM tree compaction diagram","inverted index posting list diagram"],"num_per_query":1}

### Cursor’s client-side storage layout: mmap the dictionary, stream postings

Cursor strongly argues for **local** indexing and querying on the user’s machine for three reasons:
- regex search still requires per-file scanning for verification, so server-side execution would require file synchronization or expensive client/server round trips,
- local storage sidesteps security/privacy concerns around uploading code, and
- low latency matters for agent workflows, and network round trips add friction. citeturn3view1turn3view2

Cursor also highlights a freshness constraint: a regex index needs to be “very fresh” for “read your writes” agent behavior (if the agent can’t find text it just wrote, it can waste tokens and time). citeturn3view1turn3view2

To keep editor memory usage low, Cursor stores its index in two files:
- a **postings file** containing posting lists laid out sequentially (flushed directly during construction), and
- a **sorted lookup table** mapping n-gram hashes to posting-list offsets, which is memory-mapped and queried via binary search; the postings file is then read at the returned offset. citeturn3view2

Cursor also notes that storing hashes instead of full n-grams can only broaden posting lists on collision (unlikely) but does not cause incorrect results, because correctness is enforced by verification on the underlying text. citeturn3view2

### Commit-consistent snapshots and overlays

When freshness meets performance, the core question becomes: *how do you update without destroying query latency?* Both Cursor and GitHub point to “commit-consistent” thinking:
- Cursor describes controlling index state by basing it on a Git commit and storing user/agent changes as a layer on top, which they say makes it quick to update and fast to load/synchronize. citeturn3view1
- GitHub describes designing its system so query results are consistent at commit granularity: searches should not partially include a new commit until processing is complete. citeturn9view0

A Rust implementation optimized for local agent use can apply the same principle by treating the base index as an immutable snapshot and representing edits as small overlay segments that can be cheaply rebuilt and periodically merged.

## Postings representations and query planning for speed

### Posting lists: keep them intersection-friendly

Most n-gram code search engines revolve around reading multiple posting lists and intersecting them. Cursor’s own explanation of posting lists and intersections is the canonical search-engine pattern. citeturn10view1 Zoekt’s positional trigram design emphasizes touching only a few posting lists per query by selecting “beginning” and “end” trigrams for a substring and checking their distance, and it explicitly notes you can choose trigrams with minimal match counts (e.g., prefer `qui` over `the`)—a basic but powerful query-planning heuristic. citeturn8view0

For an agent-oriented local code search engine, the key speed lever is therefore not exotic compression; it is **minimizing posting lists loaded** and **minimizing candidate documents passed to the verifier**. Cursor’s sparse n-gram approach is explicitly framed as minimizing posting lookups at query time (including by weighting rare character pairs higher). citeturn3view1

### Probabilistic adjacency filtering as a “cheap second gate”

Cursor’s locMask/nextMask design effectively inserts a probabilistic filter between “posting list intersection” and “full regex verification,” aiming to reduce the candidate set without paying the cost of storing full positional data. citeturn2view0turn3view0 GitHub reports trying “follow masks” (bitmasks for the character following the trigram) and notes that these masks can saturate too quickly, motivating sparse grams as a more robust long-term solution at scale. citeturn9view0

A practical Rust design can therefore treat these masks as an adaptive optimization: useful for common grams and short literals, but something to monitor (or constrain) to avoid the saturation/update pathologies that Cursor flags. citeturn2view0turn3view0

### Dense terms: when to use bitmap-style postings

For some tokens (very common grams, or grams in generated/minified content), posting lists can become massive. In that regime, bitmap indexes can outperform sorted lists. The Roaring bitmap research literature describes Roaring as a hybrid format (arrays + bitmaps) that can compress well and—depending on data—can make intersections dramatically faster than some RLE-based compressed bitmaps, reporting extreme speedups for intersections in some cases. citeturn6search0

That result motivates a pragmatic rule: keep “normal” postings as sorted integer lists (cheap to generate, cheap to merge), but switch to Roaring-like containers when document frequency becomes high enough that set operations dominate latency.

## Storage choices: custom segments vs embedded databases

“Fastest for reads/writes/search/index” depends on workload shape. Code search indexes are unusual because (a) reads are dominated by multi-key lookups + set operations, (b) writes are often bursty (initial index build, then incremental edits), and (c) latency goals are often “interactive,” not throughput-optimized batch queries. The most relevant comparison is therefore: **how much overhead stands between “lookup key(s)” and “read postings and intersect”?**

### Relative performance matrix for a local code-search workload

| Storage approach | Point lookups for grams | Bulk ingest / rebuild | Incremental updates | Concurrency model | Best fit in a Rust code-search engine |
|---|---|---|---|---|---|
| Custom immutable segments (mmap dictionary + postings file) | Excellent (binary search + direct reads) citeturn3view2turn8view0 | Excellent (sequential flush, parallel build) citeturn3view2turn0search21 | Good with overlays + merges citeturn0search21turn3view1 | Read-friendly (immutable files) citeturn0search21 | Core postings + gram dictionaries |
| SQLite (WAL) | Good, but general-purpose B-tree + SQL overhead citeturn7search0 | Good per transaction batching, but still general-purpose citeturn7search0turn7search8 | Serialized writers; batching helps citeturn7search34turn7search0 | One writer, many readers (WAL) citeturn7search34turn7search0 | Metadata, manifests, small maps |
| RocksDB (LSM KV store) | Good with Bloom filters, but read amplification can exist citeturn7search1turn6search5 | Excellent for write-heavy workloads; compaction tradeoffs citeturn7search1turn6search5 | Good; compaction is the cost center citeturn6search5turn7search5 | Multi-threaded; background compaction citeturn6search5turn7search1 | Optional: index build cache, manifests, auxiliary KV |
| LMDB (mmap CoW B+tree) | Excellent for read-heavy access; memory-mapped design citeturn7search2turn6search2 | Good, but single-writer serialized citeturn6search2turn7search22 | Limited by single writer; strong read behavior citeturn6search2turn7search2 | One writer, many readers citeturn6search2turn7search2 | Optional: dictionaries, symbol tables, small postings |

This table is best read alongside what the authoritative documentation emphasizes:

- SQLite’s WAL documentation explicitly highlights improved concurrency (readers don’t block writers and vice versa), generally faster performance in many scenarios, and more sequential I/O patterns under WAL. citeturn7search0turn7search12 However, SQLite WAL still enforces “one writer” at a time; a second writer waits for the first transaction to finish (a common constraint for write-heavy indexing pipelines). citeturn7search34
- RocksDB’s own tuning guide and overview emphasize the central tradeoffs of LSM-based systems: write amplification versus read amplification, and the role of Bloom filters in reducing read amplification for point lookups. citeturn7search1turn6search5
- LMDB’s documentation and presentations emphasize a single-writer/many-readers model and copy-on-write page management, enabling concurrent access with serialized writes. citeturn6search2turn7search2turn7search22

### Practical conclusion for a speed-first Rust implementation

If the goal is an “instant grep” experience (low tens of milliseconds for warm queries) and cheap repeated regex calls by an agent, the most direct path is typically **custom immutable segment files**—because they match Cursor’s and Zoekt’s “mmap-friendly layout + postings on disk” approach and minimize layers between query planning and postings intersection. citeturn3view2turn8view0turn10view0

Embedded databases can still be valuable, but mostly for what they are best at:
- SQLite for durable metadata and simple manifests with transactional safety and good read concurrency under WAL, citeturn7search0turn7search12
- RocksDB for write-heavy auxiliary KV workloads where compaction cost is acceptable and keys are naturally KV-shaped, citeturn7search1turn6search5
- LMDB for extremely fast read-mostly maps when single-writer constraints fit your update model. citeturn7search2turn6search2

## Blueprint for a Rust hybrid engine

### Index set and query pipeline

A Rust system aligned with the “hybrid” thesis and grounded in what Cursor/GitHub/Zoekt actually do would typically implement:

A **path (and filename) index** as a first-stage scope reducer. GitHub’s query rewrite explicitly includes path-related clause iterators (`paths_grams_iter…`). citeturn9view0 Zoekt’s design describes indexing filenames and storing filename posting lists (separately from content posting lists). citeturn8view0 Even scan-based tools like `ripgrep` emphasize file-type scoping (e.g., `-tpy`, `-Tjs`) as a core performance/usability feature, which is essentially a simplified form of “path/type filtering.” citeturn4search3

A **content n-gram index** as the primary candidate generator for literals and regexes, with:
- regex decomposition into required grams (AND), alternations (OR), and “match-all” fallbacks when the pattern yields no reliable grams, citeturn10view1turn8view0turn9view0
- optional phrase-awareness (Cursor’s masks) for cheaper adjacency filtering, citeturn3view0turn2view0
- optional sparse-gram tokenization to reduce query-time grams and improve selectivity, grounded in Cursor, GitHub, and the ClickHouse sparseGrams formulation. citeturn3view1turn9view0turn11view0turn4search13

A **symbol/AST index** as a secondary precision layer for supported languages, which can range from “lightweight” to “heavyweight”:

- For **syntax-level structure**, Tree-sitter is designed as an incremental parsing library that can build syntax trees and update them efficiently as files are edited. citeturn5search4turn5search0 This supports “find function definition” and structural navigation that pure text indexes cannot reliably provide.
- For **definition/reference extraction**, ctags-style systems are explicitly definition-oriented, and Universal Ctags also supports reference tags when configured. citeturn5search1turn5search5 Zoekt’s design notes symbol-definition-based ranking signals and points to `ctags` as a pragmatic (if imperfect) way to find symbol definitions during indexing. citeturn8view0
- For **semantic resolution** (e.g., “who calls this trait method?”), integrating a language server is often required; Cursor’s framing of the ecosystem highlights LSP as the standard way editor tooling externalizes semantic indexes. citeturn10view0turn5search2

A reasonable default policy is:
- always build path + content n-gram indexes (language-agnostic),
- build symbol/AST indexes opportunistically for languages where the parser/tooling is reliable and maintenance cost is acceptable,
- always run an exact verifier last.

### Index update strategy

Cursor’s constraints suggest a guiding principle: keep the regex/text index local and fresh because agent workflows are sensitive to “read-your-writes” failures. citeturn3view1turn3view2 Achieving this without slow in-place mutation generally points toward:
- immutable base snapshots keyed to a known repository state (e.g., Git commit), with overlays for working changes, citeturn3view1turn9view0
- periodic merges/compactions to bound the number of segments consulted per query (the same reason Lucene and Tantivy merge segments). citeturn0search21turn0search1turn0search10

GitHub’s “commit-consistent” query semantics reinforce the importance of atomic visibility for index updates (no partial states). citeturn9view0 A local Rust engine can replicate this by writing new segment files, then atomically swapping a manifest pointer.

### Regex verification engine safety

Because the index returns candidates and the verifier enforces correctness, verifier choice affects both performance predictability and robustness against pathological patterns.

The Rust `regex` crate explicitly omits features “not known how to implement efficiently,” including look-around and backreferences. citeturn5search15 Its documentation also emphasizes that it does **not** use unbounded backtracking, which is precisely what causes “catastrophic backtracking” risks in many traditional engines. citeturn5search23

At the same time, real users sometimes demand PCRE-style features. `ripgrep` addresses this by offering optional PCRE2 support (enabled with `-P`) to unlock look-around and backreferences, at the cost of potentially different performance characteristics. citeturn4search3 A Rust code-search engine can mirror this split: a safe, linear-time default verifier for agent-driven automation, with an explicit “unsafe/advanced regex” mode if needed.

### Benchmark targets grounded in deployed systems

Published targets from existing systems give realistic reference points:
- Zoekt’s stated goals include sub-50ms results on large codebases (multi-gigabyte corpora) on a single machine with SSD storage. citeturn8view0
- GitHub reports shard-level p99 response times on the order of 100ms in its distributed system (with end-to-end response higher due to aggregation, permissions filtering, highlighting, etc.). citeturn9view0
- Cursor claims that removing grep time from agent workflows yields meaningful iteration-time savings, particularly in large repositories where scan-based grep latency scales with repository size. citeturn3view2turn10view0

For a Rust local implementation, the most decision-relevant benchmarks are therefore:
- cold-cache vs warm-cache query latency distributions (p50/p95/p99),
- candidate set size versus pattern type (literal-heavy vs regex-heavy),
- index build throughput (bytes/sec) and incremental update latency after edits,
- resident memory cost (especially dictionary structures) and syscall/page-fault behavior.

These metrics directly reflect the tradeoffs highlighted by Cursor (local freshness and low memory via mmap tables), Zoekt (mmap-friendly shard layout and selective postings touches), and GitHub (commit-consistent query results and sparse grams to control false positives at scale). citeturn3view2turn8view0turn9view0turn3view1
