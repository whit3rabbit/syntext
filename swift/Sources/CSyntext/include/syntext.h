/* syntext.h — C ABI for the syntext `ffi` feature. Hand-written; mirrors src/ffi/.
 *
 * Conventions:
 *  - Pointer-returning fns return NULL on error and set *err_out (if non-NULL).
 *  - int32_t-returning fns return SYNTEXT_OK (0) or an error code; *err_out
 *    (if non-NULL) carries the message.
 *  - Returned char* JSON strings are owned by the caller: free with
 *    syntext_string_free. Error messages are borrowed until syntext_error_free.
 *  - All handles are thread-safe (internally synchronized); call from any
 *    thread. Rust panics are caught at this boundary and never cross it.
 *  - Strings in are NUL-terminated UTF-8. Only syntext_mem_index_add takes raw
 *    (ptr, len) bytes, which may contain NUL and invalid UTF-8.
 */
#ifndef SYNTEXT_H
#define SYNTEXT_H
#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Stable, append-only error codes. Never renumber. */
enum {
    SYNTEXT_OK                   = 0,
    SYNTEXT_ERR_IO               = 1,
    SYNTEXT_ERR_INDEX_NOT_FOUND  = 2,
    SYNTEXT_ERR_INVALID_PATTERN  = 3,
    SYNTEXT_ERR_CORRUPT_INDEX    = 4,
    SYNTEXT_ERR_QUERY_TOO_BROAD  = 5,
    SYNTEXT_ERR_PATH_OUTSIDE_REPO = 6,
    SYNTEXT_ERR_FILE_TOO_LARGE   = 7,
    SYNTEXT_ERR_LOCK_CONFLICT    = 8,  /* retryable: re-acquire with backoff */
    SYNTEXT_ERR_OVERLAY_FULL     = 9,
    SYNTEXT_ERR_DOC_ID_OVERFLOW  = 10,
    SYNTEXT_ERR_INVALID_ARGUMENT = 100, /* NULL handle, non-UTF-8 input, bad JSON */
    SYNTEXT_ERR_PANIC            = 101, /* Rust panic caught at the boundary */
    SYNTEXT_ERR_UNKNOWN          = 200  /* future IndexError variants (non_exhaustive) */
};

typedef struct syntext_index     syntext_index;     /* opaque */
typedef struct syntext_mem_index syntext_mem_index; /* opaque */
typedef struct syntext_error     syntext_error;     /* opaque */

/* ── Shared ─────────────────────────────────────────────────── */
/* Crate version as a static NUL-terminated string (never freed). */
const char *syntext_version(void);
/* Free an owned JSON string returned by any syntext fn. NULL-safe. */
void        syntext_string_free(char *s);
/* Numeric error code of err (SYNTEXT_ERR_UNKNOWN for NULL). */
uint32_t    syntext_error_code(const syntext_error *err);
/* Borrowed NUL-terminated message (NULL for NULL); valid until
 * syntext_error_free. */
const char *syntext_error_message(const syntext_error *err);
/* Free an error handle set via *err_out. NULL-safe. */
void        syntext_error_free(syntext_error *err);

/* ── Native directory index (projects) ──────────────────────── */
/* config_json: nullable ConfigJson (see docs/SWIFT.md); NULL/"{}" = defaults.
 * repo_root is walked (gitignore-aware) and the index is written to index_dir.
 */
syntext_index *syntext_index_build(const char *index_dir, const char *repo_root,
                                   const char *config_json, syntext_error **err_out);
/* Open an existing index (shared flock; LockConflict is retryable). */
syntext_index *syntext_index_open(const char *index_dir, const char *repo_root,
                                   const char *config_json, syntext_error **err_out);
/* Release an index handle. NULL-safe. */
void           syntext_index_free(syntext_index *idx);

/* Search for pattern (literal or regex). options_json: nullable
 * SearchOptionsJson. Returns an owned JSON array of MatchDto. */
char   *syntext_index_search(syntext_index *idx, const char *pattern,
                             const char *options_json, syntext_error **err_out);
/* Bounded git auto-update, then search. limits_json: nullable LimitsJson
 * (NULL = 200 files / 150 ms; "{}" = no limits). Returns
 * {"matches": [MatchDto...], "update_outcome": UpdateOutcomeDto}.
 * Requires git on PATH. */
char   *syntext_index_search_fresh(syntext_index *idx, const char *pattern,
                             const char *options_json, const char *limits_json,
                             syntext_error **err_out);
/* Index statistics as StatsDto JSON. */
char   *syntext_index_stats(syntext_index *idx, syntext_error **err_out);
/* Bounded git change detection + apply. Returns UpdateOutcomeDto JSON. */
char   *syntext_index_update_from_git(syntext_index *idx, const char *limits_json,
                             syntext_error **err_out);

/* Buffer a file change. path must be absolute, under repo_root (resolved by
 * stripping repo_root as a prefix; a bare relative path is rejected).
 * Visible to searches only after syntext_index_commit_batch. */
int32_t syntext_index_notify_change(syntext_index *idx, const char *path,
                             syntext_error **err_out);
/* Buffer a file deletion. Same path contract as notify_change. Visible only
 * after syntext_index_commit_batch. */
int32_t syntext_index_notify_delete(syntext_index *idx, const char *path,
                             syntext_error **err_out);
/* Apply all buffered notifications atomically (snapshot swap). */
int32_t syntext_index_commit_batch(syntext_index *idx, syntext_error **err_out);
/* Full checksum verification of all base segments (O(index) I/O). */
int32_t syntext_index_verify(syntext_index *idx, syntext_error **err_out);

/* ── In-memory document index (chats) ───────────────────────── */
/* Create an empty in-memory index. */
syntext_mem_index *syntext_mem_index_new(syntext_error **err_out);
/* Release a mem-index handle. NULL-safe. */
void               syntext_mem_index_free(syntext_mem_index *midx);
/* Buffer a document. content is (ptr, len) bytes: may contain NUL and invalid
 * UTF-8. doc_id must be a non-empty relative-path-shaped id (no leading '/',
 * no ".."); same-id adds replace. Binary-looking content is skipped at commit.
 * Visible to searches only after syntext_mem_index_commit. */
int32_t            syntext_mem_index_add(syntext_mem_index *midx, const char *doc_id,
                             const uint8_t *content, size_t content_len,
                             syntext_error **err_out);
/* Buffer a document deletion (absent id is a no-op). */
int32_t            syntext_mem_index_remove(syntext_mem_index *midx, const char *doc_id,
                             syntext_error **err_out);
/* Rebuild the snapshot from all buffered documents and publish it atomically.
 * O(total content); blocks add/remove for the duration. */
int32_t            syntext_mem_index_commit(syntext_mem_index *midx, syntext_error **err_out);
/* Search the committed snapshot. Returns an owned JSON array of MatchDto. */
char              *syntext_mem_index_search(syntext_mem_index *midx, const char *pattern,
                             const char *options_json, syntext_error **err_out);

#ifdef __cplusplus
}
#endif
#endif /* SYNTEXT_H */
