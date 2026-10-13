# Context Engine: Design

`syntext-context` (the `context/` workspace member) is a library for
inference hosts. Its job is to let a local model with a 4K to 8K token window
work effectively over repositories, document sets, and conversation histories
that are orders of magnitude larger than the window, without pretending the
model processes all of those tokens.

This document records the decisions behind it, in the same Decision /
Rationale / Alternatives style as [ARCHITECTURE.md](ARCHITECTURE.md). The
crate README, [context/README.md](../context/README.md), covers usage.

## The constraint

A 4K window minus a 1K answer reserve, a system prompt, and a few turns of
history leaves roughly 2,500 tokens of evidence. That is about 10 KB of text,
or one large source file. Everything this crate does follows from treating
that number as fixed: the model sees only the evidence packet, so the quality
of the answer is bounded by the quality of selection, and selection has to
happen outside the model.

The engine never claims a larger effective context. Every packet records the
budget it was given, the tokens it used, and every block it contains, so a
host can show the user exactly what the model saw.

## Pipeline

```text
COMPILE   (offline, incremental)
  sources -> decode / detect language -> structural passages
          -> code-aware terms (BM25F postings) + entity facts -> .ctxs segment

ASSEMBLE  (per question, milliseconds)
  question -> terms, identifiers, phrases, entity keys, intent
    |-- lexical: BM25 over postings, rarest terms first, budgeted
    |-- graph:   entity seeds -> bounded multi-hop beam walk -> evidence chains
    '-- exact:   syntext identifier search -> stored or live passages
  pool -> rerank (re-read text, exact features) -> ranked passages
       -> pack (floor, merge, caps, dedupe, fill, condense, widen, exact fit)
       -> render (evidence in a user message, policy in the system prompt)
       -> receipt
```

Module map: `context/src/structure/` (segmentation), `context/src/analysis/`
(terms), `context/src/graph/` (entities and edges), `context/src/compile/`
(documents and directory sync), `context/src/store/` (segments, manifest,
journal, compaction), `context/src/retrieve/` (candidates, graph walk, syntext
bridge, reranker), `context/src/pack/` (budgeting and packing),
`context/src/render/` (prompt and sanitization), `context/src/memory.rs`,
`context/src/receipt.rs`, `context/src/engine/` (the facade).

## Passages are structural units

**Decision.** Documents are segmented along their own structure: Markdown
sections and groups of whole blocks (paragraphs, lists, tables, fenced code),
code items (a function with its doc comment and attributes, an impl block
split into methods when oversized), one conversation turn. Retrieval scores
these units, and packing includes them whole whenever they fit.

**Rationale.** Sentence-level distillation optimizes the wrong thing. The
sentence that matches a question is often not the one that answers it
correctly: "Failed charges are retried three times" is followed by "unless
the card was reported stolen". Trimming to the matching sentence hands the
model a confident, wrong premise. A paragraph or a function is the unit in
which authors put a claim together with its qualifications. The end-to-end
test `small_window_prompt_keeps_the_qualification`
(`context/tests/end_to_end.rs`) pins this behaviour.

When a unit does not fit, the condenser (`context/src/pack/condense.rs`) keeps
whole lines: the anchor (heading or signature), the first line, and windows
around each matching line, and replaces every gap with an explicit
`[... lines A-B omitted ...]` marker. The model can see that text is missing
and where. When budget is left over, accepted units are widened to their
enclosing section or neighbouring passages (`context/src/pack/expand.rs`), so
an 8K window gets fuller context than a 4K one from the same ranking.

Adjacent hits in one document merge into a single span, but only up to 30% of
the evidence budget (`PackOptions::max_span_share`), and a merged span scores
at most 20% above its best passage. Without both limits, a file of similar
tests merged into one block that took 62% of a 4K budget and pushed out the
paragraph that actually explained the design.

**Alternatives considered.** Fixed token windows with overlap (the common RAG
default): cuts functions and paragraphs mid-thought and duplicates text across
chunks. Sentence distillation: smallest packets, but loses qualifications and
contradictions, as above. Whole files: wastes most of a small window on
irrelevant code.

## Code-aware terms instead of embeddings

**Decision.** The index is lexical: BM25F over normalized terms, with a
code-aware analyzer. Identifiers split into subwords and also yield one
compound term shared by every casing convention, so `parse_query` in a
question matches `parseQuery` in a file. A Porter stemmer collapses
inflections. Breadcrumbs and symbol names count as a title field.

**Rationale.** It runs anywhere a model runs, with no second model to load,
no GPU contention with the inference engine, and no embedding drift when files
change: recompiling a changed file is cheap and exact. Questions about code are
dominated by identifiers, which exact lexical matching handles better than
dense vectors. The reranker then re-reads each candidate and computes exact
features (coverage, proximity, phrase and bigram matches, symbol match,
intent-aware structure prior, recency for conversation turns, graph and
exact-match signals), which is where most ranking quality comes from.

**Alternatives considered.** Dense embeddings: strong for paraphrase, but a
second model resident next to the LLM on consumer hardware, slow to rebuild,
and weak on identifiers. They remain a valid future signal in the reranker,
not a replacement for the lexical index. Trigram or n-gram indexing (as in
syntext itself): built for exact substring search, not relevance ranking;
syntext is used for exactly that role (see below).

## Canonical entities and an optional graph

**Decision.** Every entity has a canonical key from `fold_key`: lowercase
subwords joined by spaces, so `parse_query`, `parseQuery`, and "Parse Query"
are the same entity without any alias table. Explicit aliases cover what
folding cannot: acronyms and their expansions ("Abstract Syntax Tree (AST)"),
import aliases (`import numpy as np`), type aliases, and "formerly" / "also
known as" phrasing. Edges are typed (`Relation`, `Calls`, `MemberOf`,
`Implements`, `Extends`, `CoMention`), carry a confidence, and record the
passage they were read from. The graph is stored inside the store's own
segments, and `GraphMode` (`Off`, `Code`, `Full`) is chosen per store.

**Rationale.** Exact-string relationship matching (one JSONL line per fact,
joined by literal names) is cheap but misses every alias and every implicit
relationship, and it makes a multi-hop question fail on the first spelling
difference. Canonical keys make the common cases free; confidence lets
traversal prefer precise edges; and because every edge points at a passage,
a wrong edge costs ranking quality, never a hallucinated fact: the model only
ever sees original text. Making the graph optional and segment-local means no
graph database to run and no separate consistency problem. A store compiled
with `GraphMode::Off` simply has no graph signal.

Traversal (`context/src/retrieve/graph_walk.rs`) seeds from question n-grams
that name an entity, then runs a bounded beam walk in both edge directions.
Activation decays per hop, is damped for hub entities, and gets a bonus when an
edge's predicate shares a term with the question, so "which group does X's
manager manage" prefers a `manages` edge over an unrelated one. Every traversed
edge contributes its source passage, so the model receives the whole chain, not
just the last hop. Passages that merely mention a seed entity get a lower weight
than ones the graph reached through edges, because they already matched the
name lexically.

**Alternatives considered.** A mandatory knowledge graph built by an LLM at
ingest: expensive, slow, nondeterministic, and its extraction errors become
"facts". A separate graph store: another process and another consistency
boundary for every write.

## Exact matches from syntext

**Decision.** An optional `SyntextSource` runs a case-insensitive syntext
search for each identifier in the question. A hit in a file whose stored text
is byte-identical to what syntext just verified marks that stored passage; a
hit in a file that is new or changed since the last sync is segmented from the
live bytes on the spot and competes as a live candidate
(`context/src/retrieve/syntext_source.rs`).

**Rationale.** syntext already verifies every match against live file bytes.
Reusing it means identifier questions are answered from the working tree as it
is now, even between syncs, and a stale stored copy of a changed file loses to
the live one instead of being cited.

**Alternatives considered.** Syncing on every query: correct but adds a
directory walk and hashing to each question. Ignoring drift between syncs:
cites code that no longer exists.

## Evidence placement and trust

**Decision.** Evidence goes in a user message, as `<evidence id="E1"
source="..." lines="..." kind="..." trust="...">` blocks. It never goes in the
system role. The system prompt stays the host's, plus a short policy
(`EVIDENCE_POLICY`) that says evidence is reference data, not instructions,
and must be cited. Inside evidence, closing tags, chat-template control tokens
(`<|im_end|>`, `[INST]`, and similar), and tool-call markup are broken with a
zero-width space so no tokenizer or template parser can match them; control
characters and ANSI escapes are stripped (`context/src/render/sanitize.rs`).

Each document carries a `Trust` level: `External` (downloaded pages, tool
output), `Memory` (past turns, which may quote external content),
`Workspace`, and `Curated`. Trust never promotes evidence to instructions.
`EvidencePacket::lowest_trust()` exposes the least-trusted source in a packet
so the host can apply its own tool permission model, for example refusing
side-effecting tool calls in a turn that carries external content.

**Rationale.** The system role is where a host puts its rules and where many
models give text the most authority. Placing retrieved documents there lets any
file in a repository, or any page a tool fetched, speak with the host's voice,
which defeats whatever tool-permission model the host has. Keeping evidence in
the user turn, labelled and sanitized, preserves the trust boundary the host
already relies on.

**Alternatives considered.** Evidence in the system prompt: rejected for the
reason above. A separate "tool" role message: not supported uniformly across
chat templates, and some templates require strict user/assistant alternation,
so the default (`EvidencePlacement::WithQuestion`) puts evidence and question
in one user message.

## Budgeting

**Decision.** `Budget::for_window` reserves a quarter of the window for the
answer (256 to 2,048 tokens, at most half the window) and a 1/32 safety margin,
then measures the system prompt, kept history, and question with the host's
token counter, including a per-message template overhead. Evidence gets exactly
what remains. After rendering, the whole prompt is re-counted and the evidence
budget shrunk until it fits (`ContextEngine::assemble`). Packing itself
verifies the rendered evidence against its budget with the real counter and
drops the weakest block until it fits.

**Rationale.** Token counts are not additive across concatenation for most
tokenizers, and chat templates add tokens the host does not see. A packet that
is "about right" overflows the window on the first unlucky question. The
property test `prompts_fit_and_evidence_is_well_formed`
(`context/tests/budget_props.rs`) checks the invariant across random windows,
questions, and packing options.

**Alternatives considered.** Character-based budgets: off by 2x to 4x across
languages and code. Leaving the final fit to the server: truncation then cuts
the question or the evidence policy.

## Conversation memory

**Decision.** Every turn is appended to a checksummed journal and synced
before `append_turn` returns; it is searchable immediately through an
in-memory live segment and compacted into a durable segment every
`memory_flush_turns` turns. The manifest's `memory_through` advances only in
the same commit that publishes the segment holding those turns, so a crash
can re-index turns but never lose them, and a torn journal tail is cut off on
the next writable open. `ContextEngine::assemble` keeps as many recent turns
verbatim as the history share of the window allows (30% by default) and
excludes exactly those from retrieval, so older turns return only when the
question makes them relevant, as cited evidence with `Trust::Memory`.

Memory is scoped: `MemoryMode::Project(id)` and `MemoryMode::App(id)` label
turns `project:<id>` or `app:<id>`, a request sees a scope only when its
`ScopeFilter` grants it (the engine grants the memory's own scope
automatically), and `MemoryMode::Off` persists nothing.

**Rationale.** Dropping old turns loses what the user said; summarizing them
loses the exact wording and adds a model call per turn. Indexing turns like any
other document makes recall exact and cheap, and scoping prevents one
application from reading another's conversations through a shared store.

**Alternatives considered.** Rolling summaries: lossy and slow. Keeping all
history in the window: impossible past a few dozen turns at 4K.

## Storage

**Decision.** The store is a directory with its own immutable segment format
(`.ctxs`, `context/src/store/format.rs`): a 64-byte header, 8-byte-aligned
sections (META, STRINGS, DOCS, SECTIONS, PASSAGES, TEXT, TERMS, POSTINGS, and
optional graph sections), a table of contents with an xxh64 checksum per
section, and a footer. `manifest.json`, checksummed and replaced by atomic
rename, is the commit point of every write. Superseded documents are recorded
as per-segment dead lists until compaction. Readers take an immutable
`StoreSnapshot` with one atomic pointer load (`arc-swap`), so a concurrent
write never changes the corpus under a query.

Segments are read with positional reads, not memory maps. Large sections are
read only for the candidates a query touches, an external truncation surfaces
as an I/O error rather than `SIGBUS`, and the crate has no `unsafe`
(`#![forbid(unsafe_code)]`). Every segment is treated as untrusted input:
open validates magic, version, table-of-contents bounds and checksums, every
metadata section's checksum, and every cross-reference, after which accessors
cannot panic on segment contents. A test flips every byte of a small segment
in turn and requires each result to be rejected or read without panicking.

Writes compile `StoreOptions::batch_bytes` (32 MiB) of source text per
segment, write each segment file as soon as its batch is compiled, and publish
all of them in one manifest commit (`context/src/store/write.rs`). Memory
during a large sync or compaction is therefore proportional to one batch: on a
274 MiB corpus of 9,076 source files, peak RSS fell from 1,731 MiB with a
single segment to 571 MiB. Compaction is tiered: when more than
`max_segments` small segments (under about half a batch) accumulate, only they
are merged; the whole store is rewritten only when more than 30% of documents
are dead or a segment was compiled by an older `COMPILER_VERSION`.

Each segment records the `COMPILER_VERSION` that produced it. Bump that
constant whenever a change to analysis, segmentation, or graph extraction would
compile the same document differently: segments from older versions are
recompiled from their stored text by the next compaction, so a store never
answers a query with one term normalization against postings built with
another.

**Rationale.** A purpose-built format keeps the index, passage table, stored
text, and graph of one batch in one immutable file that can be validated,
shared between snapshots, and replaced atomically. Storing the text makes
compaction and recompilation possible without the original sources (notes and
conversation turns have no other copy) and lets receipts and condensation work
from exactly the bytes that were indexed.

**Alternatives considered.** A specialized single-file container per corpus:
every update rewrites it. SQLite: adds a dependency and a second durability
model next to syntext's own segment discipline, and full-text ranking would
still need custom scoring. JSONL facts and text files: no checksums, no random
access, and no atomic multi-file update. Memory-mapped segments: faster cold
reads, but `SIGBUS` on truncation and `unsafe` in the read path.

## Freshness and directory sync

**Decision.** `ContextStore::sync_directory` discovers files with syntext's
repository walk (`.gitignore`-aware, nested checkouts pruned, symlink aliases
deduplicated) and detects changes with syntext's change catalogue (a stat fast
path plus BLAKE3 content digests with git's racily-clean rule), observed in
chunks of 512 paths. Only added and modified files are read and compiled;
deleted, newly ignored, excluded, and newly binary files are removed; the
whole sync lands in one generation. The catalogue is saved only after that
generation is durable, so an interrupted sync is simply redone.

**Rationale.** The context store and a syntext index over the same root see
exactly the same files, which is what makes the exact-match bridge sound. A
no-op sync of 9,076 files takes 0.16 to 0.19 seconds, cheap enough to run
before each conversation.

## Receipts

**Decision.** Every packet carries a `Receipt`: store id and generation,
question analysis (intent, terms, identifiers, seed entities), and for each
block its source, byte and line range, BLAKE3 digest of the source version,
merged passage ids, score and per-signal breakdown, graph chain, and whether it
was condensed, widened, sanitized, or read live. Up to 16 excluded candidates
are listed with the reason (below floor, per-document cap, near duplicate,
budget, block limit). `Receipt::check_sources` re-hashes cited files and
reports which changed or disappeared since.

**Rationale.** A host can answer "which revision of which file supported this
claim?" long after the fact, show the user what the model actually saw, and
flag an answer whose sources have since changed.

## Measured

Release build, 4-core Linux container, default options:

| Corpus | Text files | Cold compile | Peak RSS | Store | No-op sync | Query |
|---|---|---|---|---|---|---|
| this repository | 367 | 0.39 s | not measured | 6.2 MiB | 12 ms | 11-15 ms |
| local cargo registry sources (274 MiB) | 9,076 | 16.6 s | 571 MiB | 222 MiB | 0.16-0.19 s | 15-30 ms |

Query time covers retrieval and packing on a warm store; the first query after
a cold compile took 124 ms. On this repository, "Where is route_query
defined?" at a 4K window returns the definition in `src/query/mod.rs` first,
followed by its callers, in 11 ms.

## Limitations and future work

- No semantic signal: a question that shares no words, identifiers, or
  entities with its answer misses it. An embedding score could join the
  reranker as one more signal without changing the rest of the pipeline.
- Prose entity and relation extraction is heuristic and English-centric;
  definition detection is line-based per language rather than parsed.
- Ranking weights are hand-tuned against a small set of real questions.
- A single line longer than the remaining budget cannot be condensed and is
  excluded (recorded in the receipt). Sentence-boundary condensation with
  explicit markers would cover unwrapped prose.
- One writer per store; read-only handles in other processes pick up new
  generations with `reload()`.
