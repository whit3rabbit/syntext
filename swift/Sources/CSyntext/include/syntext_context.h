/* syntext_context.h — C ABI of the syntext-context engine (`ffi` feature of
 * the syntext-context crate). Hand-written; mirrors context/src/ffi/.
 *
 * Shares syntext.h's conventions and types, and is compiled into the same
 * static library, so an application links one Rust library:
 *  - Pointer-returning fns return NULL on error and set *err_out (if
 *    non-NULL); free errors with syntext_error_free.
 *  - Returned char* JSON strings are owned by the caller: free with
 *    syntext_string_free. JSON in and out is snake_case; unknown input
 *    fields are ignored and output fields are only ever added.
 *  - Handles are thread-safe; call from any thread. Calls block: keep them
 *    off a UI thread. Engine and memory handles keep their store alive, so
 *    free handles in any order. All *_free fns are NULL-safe.
 *  - Rust panics are caught at this boundary (SYNTEXT_ERR_PANIC).
 * See docs/CONTEXT.md for the JSON shapes and an integration guide.
 */
#ifndef SYNTEXT_CONTEXT_H
#define SYNTEXT_CONTEXT_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "syntext.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Context error codes: stable, append-only, never renumbered. I/O errors use
 * SYNTEXT_ERR_IO, bad arguments SYNTEXT_ERR_INVALID_ARGUMENT, and errors from
 * an attached syntext index keep their syntext codes. */
enum {
    SYNTEXT_CONTEXT_ERR_LOCKED              = 300, /* another process holds the store's writer lock: retryable */
    SYNTEXT_CONTEXT_ERR_CORRUPT             = 301, /* a store file failed validation */
    SYNTEXT_CONTEXT_ERR_NOT_FOUND           = 302, /* no store at the directory */
    SYNTEXT_CONTEXT_ERR_READ_ONLY           = 303, /* write through a read-only handle */
    SYNTEXT_CONTEXT_ERR_UNSUPPORTED_VERSION = 304, /* store written by an incompatible version */
    SYNTEXT_CONTEXT_ERR_WINDOW_EXCEEDED     = 305, /* system prompt + question alone exceed the window */
    SYNTEXT_CONTEXT_ERR_CANCELLED           = 306, /* sync cancelled from its progress callback */
    SYNTEXT_CONTEXT_ERR_EMBEDDER            = 307  /* the host embedding callback failed or returned bad vectors */
};

typedef struct syntext_context_store  syntext_context_store;  /* opaque */
typedef struct syntext_context_engine syntext_context_engine; /* opaque */
typedef struct syntext_context_memory syntext_context_memory; /* opaque */
typedef struct syntext_context_embedder syntext_context_embedder; /* opaque */

/* Sync progress, called after each chunk of observed paths on the syncing
 * thread. Return true to continue, false to cancel (the sync then fails with
 * SYNTEXT_CONTEXT_ERR_CANCELLED and publishes nothing). */
typedef bool (*syntext_context_progress_fn)(void *userdata, uint64_t observed,
                                            uint64_t total, uint64_t changed);

/* Token count of the len UTF-8 bytes at text (not NUL-terminated) with the
 * model's tokenizer. May be called from any thread, concurrently. */
typedef uint64_t (*syntext_context_count_fn)(void *userdata, const char *text,
                                             size_t len);

/* Embedding: write count*dims floats to out, one vector per text, for the
 * count texts (texts[i] is lens[i] UTF-8 bytes, not NUL-terminated);
 * is_query is true for a question. Return true on success. May be called
 * from any thread, concurrently. */
typedef bool (*syntext_context_embed_fn)(void *userdata,
                                         const char *const *texts,
                                         const size_t *lens, size_t count,
                                         bool is_query, float *out);

/* Context engine version (static; never freed). */
const char *syntext_context_version(void);
/* The evidence policy engines append to the system prompt on turns with
 * evidence (static; never freed). To keep a constant system prompt (KV-cache
 * reuse), include it once yourself and pass "append_policy": false. */
const char *syntext_context_evidence_policy(void);

/* ── Stores ─────────────────────────────────────────────────── */
/* Open the store at dir, creating it when absent. options_json (nullable):
 * {"read_only", "must_exist", "sync", "batch_bytes", "max_segments",
 *  "max_dead_ratio", "memory_flush_turns", "verify_on_open",
 *  "graph": "full"|"code"|"off"}. */
syntext_context_store *syntext_context_store_open(const char *dir,
                                                  const char *options_json,
                                                  syntext_error **err_out);
void syntext_context_store_free(syntext_context_store *store);

/* Sync a directory source: {"label", "root", "scope"?, "trust"?,
 * "max_file_bytes"?, "include_nested_checkouts"?, "exclude"?}. progress may be
 * NULL. Returns SyncReport JSON {"generation", "scanned", "added",
 * "modified", "removed", "skipped", "uncertain"}. */
char *syntext_context_store_sync(const syntext_context_store *store,
                                 const char *source_json,
                                 syntext_context_progress_fn progress,
                                 void *userdata, syntext_error **err_out);
/* Sync only paths_json (array of absolute paths under the root, or relative
 * ones) without walking the directory. Returns SyncReport JSON. */
char *syntext_context_store_sync_paths(const syntext_context_store *store,
                                       const char *source_json,
                                       const char *paths_json,
                                       syntext_error **err_out);
/* Upsert host documents: [{"id", "text", "external"?, "title"?, "trust"?,
 * "scope"?, "timestamp"?}] (keys "note:<id>", or "ext:<id>" when external).
 * Returns IngestReport JSON. */
char *syntext_context_store_upsert(const syntext_context_store *store,
                                   const char *docs_json,
                                   syntext_error **err_out);
/* Remove documents by key (JSON array of keys). Returns IngestReport JSON. */
char *syntext_context_store_remove(const syntext_context_store *store,
                                   const char *keys_json,
                                   syntext_error **err_out);
/* Compact when due (force=false) or always. Returns CompactReport JSON, or
 * "null" when nothing was due. */
char *syntext_context_store_compact(const syntext_context_store *store,
                                    bool force, syntext_error **err_out);
/* StoreStats JSON. */
char *syntext_context_store_stats(const syntext_context_store *store,
                                  syntext_error **err_out);
/* Read-only handles: pick up generations another process published.
 * *changed_out (nullable) reports whether anything changed. */
int32_t syntext_context_store_reload(const syntext_context_store *store,
                                     bool *changed_out,
                                     syntext_error **err_out);

/* ── Embeddings (semantic retrieval) ─────────────────────────── */
/* Wrap a host embedding model. id names the model and every setting that
 * changes its vectors (vectors are compared only within one id); dims is
 * the vector length (1-4096). userdata must stay valid until this handle
 * and every engine created with it are freed. */
syntext_context_embedder *syntext_context_embedder_new(
    const char *id, size_t dims, syntext_context_embed_fn embed,
    void *userdata, syntext_error **err_out);
void syntext_context_embedder_free(syntext_context_embedder *embedder);
/* Give live passages vectors, prose first, reusing the vector of any text
 * embedded before (sync and compaction never re-embed unchanged text).
 * Resumable: a budget, a cancel, or an error keeps what was embedded.
 * options_json (nullable): {"batch"?, "max_passages"?,
 * "prune_other_models"?}. progress (nullable) gets (userdata, done, total,
 * 0) after each batch; false stops early ("cancelled": true). Returns
 * {"model", "embedded", "reused", "covered", "remaining", "cancelled"}. */
char *syntext_context_store_embed(const syntext_context_store *store,
                                  const syntext_context_embedder *embedder,
                                  const char *options_json,
                                  syntext_context_progress_fn progress,
                                  void *userdata, syntext_error **err_out);

/* ── Engines ────────────────────────────────────────────────── */
/* options_json (nullable): {"model"?, "tokenizer"?, "syntext"?: {"index_dir",
 * "repo_root", "label", "scope"?, "trust"?}, "hops"?, "max_results"?,
 * "max_blocks"?, "per_doc_cap"?, "relevance_floor"?, "expand"?,
 * "order": "ranked"|"document", "placement": "with_question"|"separate_message",
 * "append_policy"? (default true)}.
 * count (nullable) counts tokens with the model's tokenizer; count_userdata
 * must stay valid until the engine is freed. embedder (nullable) adds
 * semantic retrieval over vectors stored with the same embedder; the engine
 * keeps its own reference. */
syntext_context_engine *syntext_context_engine_new(
    const syntext_context_store *store, const char *options_json,
    syntext_context_count_fn count, void *count_userdata,
    const syntext_context_embedder *embedder, syntext_error **err_out);
void syntext_context_engine_free(syntext_context_engine *engine);

/* Evidence within an explicit budget, for hosts that build the prompt
 * themselves. memory (nullable) recalls older turns of that conversation:
 * its scope is granted and its "kept_turns" most recent turns (already
 * verbatim in the host's prompt) are left out. request: {"question",
 * "evidence_tokens", "kept_turns"?, "scopes"?, "exclude"?}. Returns
 * {"blocks", "evidence_text", "evidence_tokens", "budget", "lowest_trust",
 * "receipt", "receipt_json"}. */
char *syntext_context_prepare(const syntext_context_engine *engine,
                              const syntext_context_memory *memory,
                              const char *request_json,
                              syntext_error **err_out);
/* A whole prompt for a window. memory (nullable) supplies history and its
 * scope. request: {"question", "window", "system"?, "history"?: [{"role",
 * "content"}], "generation_reserve"?, "history_share"?, "scopes"?,
 * "exclude"?}. Returns {"messages", "packet", "budget", "history_kept",
 * "history_dropped", "prompt_tokens"}, or fails with
 * SYNTEXT_CONTEXT_ERR_WINDOW_EXCEEDED rather than exceed the window. */
char *syntext_context_assemble(const syntext_context_engine *engine,
                               const syntext_context_memory *memory,
                               const char *request_json,
                               syntext_error **err_out);
/* Ranked passages for a search tool: {"question", "max_results"?,
 * "scopes"?, "exclude"?}. Returns [{"doc_key", "source", "breadcrumb",
 * "symbol", "kind", "trust", "scope", "lines", "score", "text", "live"}]. */
char *syntext_context_search(const syntext_context_engine *engine,
                             const char *request_json,
                             syntext_error **err_out);

/* ── Conversation memory ────────────────────────────────────── */
/* Open a conversation and reload its turns. mode_json: {"project": "<id>"},
 * {"app": "<id>"}, or "off". */
syntext_context_memory *syntext_context_memory_open(
    const syntext_context_store *store, const char *session,
    const char *mode_json, syntext_error **err_out);
void syntext_context_memory_free(syntext_context_memory *memory);
/* Durably record a turn; role: "user"|"assistant"|"system"|"tool". */
int32_t syntext_context_memory_record(const syntext_context_memory *memory,
                                      const char *role, const char *text,
                                      syntext_error **err_out);
/* Every turn, oldest first: [{"role", "content"}]. */
char *syntext_context_memory_history(const syntext_context_memory *memory,
                                     syntext_error **err_out);

/* ── Receipts ───────────────────────────────────────────────── */
/* Re-hash the files a receipt cites (keys "<label>:<path>" under root).
 * Returns [{"id", "doc_key", "status"}], status unchanged|changed|missing|
 * not_checked. */
char *syntext_context_receipt_check(const char *receipt_json, const char *root,
                                    const char *label, syntext_error **err_out);

#ifdef __cplusplus
}
#endif
#endif /* SYNTEXT_CONTEXT_H */
