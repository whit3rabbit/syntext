# syntext-context

A retrieval-driven context engine for local inference. It lets a model with a
4K to 8K token window work over repositories, document sets, and conversation
histories far larger than that window, without pretending the model reads all
of it.

The model only ever sees what fits in its window. Everything else happens
before the call, in deterministic Rust: sources are compiled once into a local
store, and each question gets a token-budgeted packet of cited evidence chosen
from it. Every packet comes with a receipt saying exactly which bytes of which
source version the model was given.

```text
COMPILE   sources -> structural passages -> lexical index + entity graph -> segments
RETRIEVE  question -> lexical + exact (syntext) + graph candidates -> rerank
ASSEMBLE  ranked passages -> token-budgeted evidence packet -> chat messages
PERSIST   conversation turns -> durable journal -> indexed memory
EXPLAIN   evidence packet -> receipt with source hashes, ranges, and signals
```

The crate lives in the syntext workspace and builds on it: directory discovery
is syntext's repository walk, change detection is syntext's BLAKE3 change
catalogue, and an optional syntext index supplies exact identifier matches
verified against the live working tree.

How it works, integration guides for Rust, Swift, and C (including apps that
already ship their own Rust library), and the design reasoning with the
alternatives that were rejected are in [docs/CONTEXT.md](../docs/CONTEXT.md).

## Quick start

```rust
use std::sync::Arc;
use syntext_context::{
    ChatRequest, ContextEngine, ContextStore, ConversationMemory, DirectorySource, MemoryMode,
    StoreOptions,
};

// A store is a directory. Compile a repository into it (incremental after
// the first run: only files whose content changed are recompiled).
let store = Arc::new(ContextStore::open_or_create(".context", StoreOptions::default())?);
store.sync_directory(&DirectorySource::new("repo", ".")?)?;
store.maybe_compact()?;

// Conversation memory that persists across sessions of one project.
let memory = ConversationMemory::resume(
    Arc::clone(&store),
    "session-1",
    MemoryMode::Project("my-app".into()),
)?;

// Assemble a prompt for a model with a 4096-token window.
let engine = ContextEngine::builder(Arc::clone(&store)).build();
let turn = engine.assemble(
    &ChatRequest::new("Where is the retry backoff computed?", 4096)
        .system("You are a careful coding assistant.")
        .memory(&memory),
)?;

// `turn.messages` are OpenAI-compatible {role, content} objects: send them
// to llama.cpp's server, vLLM, Ollama, or any chat-completions endpoint.
let body = serde_json::json!({ "messages": turn.messages, "max_tokens": turn.budget.generation });

// After the model answers, record the exchange.
memory.record_exchange("Where is the retry backoff computed?", "In src/retry.rs [E1].")?;

// Provenance: what the model saw, and whether those files have changed since.
println!("{}", turn.packet.receipt.to_json());
```

For an exact budget, give the engine the model's own tokenizer through
`EngineBuilder::token_counter` (any `Fn(&str) -> usize` works via `FnCounter`).
The built-in `HeuristicCounter` errs toward over-counting so prompts fit
before a tokenizer is wired in.

To add exact identifier matches verified against live files, attach a syntext
index over the same directory, under the same label as its `DirectorySource`.
Open it through the re-exported `syntext_context::syntext`, so the index type
matches the one the engine was built with:

```rust
use syntext_context::{syntext, SyntextSource};

let config = syntext::Config::new(".syntext".into(), ".".into());
let index = Arc::new(syntext::index::Index::open(config)?);
let engine = ContextEngine::builder(Arc::clone(&store))
    .syntext(SyntextSource::new(index, "repo")?)
    .build();
```

A host that builds its own prompt (its own history, template, or prompt
cache) asks for evidence only, and places `packet.evidence_text` in the last
user message. With `memory`, older turns are recalled while the `kept_turns`
the host already has verbatim are left out:

```rust
use syntext_context::PrepareRequest;

let packet = engine.prepare(&PrepareRequest::new(question, 2400).memory(&memory, kept_turns))?;
```

## Semantic retrieval

Bring your own embedding model: implement `Embedder` (or wrap a closure in
`FnEmbedder`), embed in the background after syncs, and attach it to the
engine. Vectors are stored as 8-bit sidecars keyed by the exact text
embedded, so syncs and compaction never send unchanged text to the model
again.

```rust
use syntext_context::EmbedOptions;

let report = store.embed(encoder.as_ref(), &EmbedOptions::default())?; // resumable
let engine = ContextEngine::builder(Arc::clone(&store)).embedder(encoder).build();
```

With bge-small-en-v1.5, paraphrased questions in the evaluation set went
from 42% to 100% of answers reaching the prompt, code questions from 88% to
92%, and HotpotQA from 88% to 95%; no set dropped.

## Agent tools

Three read-only tools let the model look again after the first packet:
`context_search` (search in its own words), `context_read` (whole lines of
a source, with where to continue), and `context_outline` (the documents
available, or a document's sections and definitions with line ranges).
Results are sanitized evidence within a token budget, numbered after the
citations already given; scopes come from the app, never from the model.

```rust
use syntext_context::{tool_definitions, ToolOptions};

let defs: Vec<_> = tool_definitions().iter().map(|d| d.openai()).collect(); // offer to the model
let mut tools = ToolOptions::default();
tools.record_packet(&packet);
let result = engine.call_tool(&call.name, &call.arguments, &tools)?; // each call the model makes
tools.record(&result);
```

`examples/mcp.rs` serves the same tools to any Model Context Protocol
client over stdio.

## Swift and C

With the `ffi` feature the crate exports a C ABI
(`swift/Sources/CSyntext/include/syntext_context.h`), compiled into the same
static library as syntext's own. The Swift package wraps it as the
`SyntextContext` product:

```swift
import SyntextContext

let store = try SyntextContextStore(directory: storeDir)
try store.sync(SyntextDirectorySource(label: "repo", root: projectPath))
let engine = try SyntextContextEngine(store: store, tokenCounter: { tokenizer.count($0) })
let chat = try SyntextConversation(store: store, session: chatID, mode: .project("my-app"))
let turn = try engine.assemble(
    SyntextChatRequest(question: question, window: 8192), conversation: chat)
// turn.messages -> the model; then:
try chat.recordExchange(question: question, answer: answer)
```

## Try it

```sh
cargo run -p syntext-context --release --example ask -- . "Where is route_query defined?" --window 4096 --syntext
```

Prints the chat messages that would be sent to the model and a summary:

```text
window 4096: prompt ~3022 tokens, evidence 2869/2902 tokens in 6 blocks, intent Locate, 200 candidates, 11.3 ms
  [E1] src/query/mod.rs lines 101-138 score 0.980 (defines route_query)
  [E2] src/search/mod.rs lines 95-120 score 0.765 via 1-hop chain
  [E3] tests/unit/query.rs lines 209-249 score 0.752 via 1-hop chain
  ...
```

Add `--receipt` for the full JSON receipt, `--store <dir>` to choose where
the store lives (default: a directory under the system temp dir).

`cargo run -p syntext-context --release --example mcp -- <dir>` serves the
agent tools for a directory as a stdio Model Context Protocol server.

## What it does differently

- **Whole units, not sentences.** Passages follow the source's structure:
  Markdown sections and paragraph groups, functions with their doc comments,
  impl blocks split into methods, one conversation turn. Retrieval ranks these
  units whole, so an "unless", a negation, or a date in the next sentence stays
  attached to the claim it qualifies. When a unit does not fit, it is condensed
  to its anchor plus windows around the matching lines, with explicit
  `[... lines 40-52 omitted ...]` markers, never silently trimmed.
- **Canonical entities, optional graph.** `parse_query`, `parseQuery`, and
  "Parse Query" are one entity by construction. Aliases cover what folding
  cannot (acronyms, `import numpy as np`, "Northstar, formerly Polaris").
  Typed edges (calls, member of, implements, extends, prose relations) carry a
  confidence and the passage they were read from, so following an edge always
  yields evidence the model can see. The graph lives inside the store's own
  segments and can be turned off per store (`GraphMode::Off`).
- **Evidence is lower-trust data, never instructions.** Retrieved text goes in
  a user message inside `<evidence>` blocks with provenance attributes. The
  system prompt stays the host's and only gains a short policy saying evidence
  is reference data. Template control tokens, tool-call markup, and wrapper
  tags inside evidence are neutralized. Each block carries a trust level
  (`external`, `memory`, `workspace`, `curated`), and
  `EvidencePacket::lowest_trust()` lets the host keep its tool permission model,
  for example by disabling side-effecting tools when external content is in
  the prompt.
- **Budget honesty.** `Budget::for_window` reserves the answer and a margin,
  then measures the system prompt, kept history, and question; evidence gets
  exactly what is left, and the assembled prompt is re-counted and shrunk until
  it fits. Leftover budget widens accepted passages to their section or
  neighbours, so an 8K window gets fuller context than a 4K one from the same
  ranking.
- **Fresh answers.** A directory sync re-reads only files whose content changed.
  With a syntext source attached, an identifier match in a file edited since
  the last sync is segmented from the live bytes on the spot.
- **Conversation memory that survives.** Turns are journaled durably
  (checksummed, torn-tail safe), searchable immediately, and compacted into
  segments every `memory_flush_turns` turns. Recent turns stay verbatim in
  the prompt; older ones come back only when relevant, as cited evidence. Memory is scoped
  (`MemoryMode::Project`, `MemoryMode::App`, `MemoryMode::Off`) so two
  applications sharing a store do not read each other's conversations.
- **Receipts.** Every packet records source, byte and line range, BLAKE3 digest
  of the source version, score breakdown, graph chain, and whether a block was
  condensed, widened, sanitized, or read live. `Receipt::check_sources` reports
  which cited files changed since.

## Measured

Release build, 4-core Linux container, default options, `HeuristicCounter`.

| Corpus | Text files | Cold compile | Peak RSS (compile) | Store | No-op sync | Query (retrieve + pack) |
|---|---|---|---|---|---|---|
| this repository | 367 | 0.39 s | not measured | 6.2 MiB | 12 ms | 11-15 ms |
| local cargo registry sources (274 MiB on disk) | 9,076 | 16.6 s | 571 MiB | 222 MiB | 0.16-0.19 s | 15-30 ms |

Query times are for a warm store; the first query after a cold compile took
124 ms on the registry corpus. Writes compile 32 MiB of source text per
segment (`StoreOptions::batch_bytes`), which is what bounds compile memory:
compiling the registry corpus into a single segment peaked at 1,731 MiB.

## Store layout

```text
<store>/
  manifest.json        live segment list, generation, compile options (checksummed)
  seg-*.ctxs           immutable segments in the crate's own binary format
  journal.log          conversation turns not yet compacted into a segment
  sources/<label>/     per-directory-source change catalogue (syntext)
  write.lock           exclusive writer lock
```

One writer per store (other processes get `ContextError::Locked`); any number
of `open_read_only` readers, which pick up new generations with `reload()`.

## Limitations

- Without an embedder, retrieval is lexical, structural, exact-match, and
  graph based, so a question that shares no words, identifiers, or entities
  with its answer will miss it (42% of the `paraphrase` evaluation set is
  found; see [context/eval/README.md](eval/README.md)). With one, the first
  embedding of a large tree takes minutes on a CPU encoder.
- Prose entity and relation extraction is heuristic and tuned for English.
- Definition detection is line-based per language (no parser), and the
  ranking weights are hand-tuned.
- A single line longer than the remaining budget (an unwrapped paragraph,
  minified code) cannot be condensed and is left out, recorded in the receipt
  as a budget exclusion.
- The agent tools help only when the model searches for something new: a
  follow-up that repeats the question found nothing the packet missed.
  `context_read` and `context_outline` read the store as of the last sync.

## Development

```sh
cargo run -p syntext-context --release --example eval -- --failures   # evaluation sets (context/eval/)
cargo test -p syntext-context
cargo test -p syntext-context --features ffi
cargo clippy -p syntext-context --all-targets --no-deps -- -D warnings
cargo clippy -p syntext-context --all-targets --features ffi --no-deps -- -D warnings
./swift/Scripts/build-xcframework.sh && (cd swift && swift test)   # macOS
```
