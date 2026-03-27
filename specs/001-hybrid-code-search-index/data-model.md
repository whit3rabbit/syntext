# Data Model: Hybrid Code Search Index

**Date**: 2026-03-25
**Branch**: `001-hybrid-code-search-index`

## Entities

### Document (File)

Represents a single indexed file in the repository.

| Field | Type | Description |
|---|---|---|
| doc_id | u32 | Unique ID within a segment, assigned at index time |
| path | String | Relative path from repo root |
| content_hash | u64 | Hash of file content for change detection |
| size_bytes | u64 | File size |
| language | Option\<String\> | Detected language (for symbol indexing) |

**Constraints**: doc_id is unique within a segment. path is unique within the manifest (across all active segments + overlay). Maximum file size for indexing: configurable, default 10MB.

---

### Segment

Immutable unit of the index, stored as a single mmap-friendly file.

| Field | Type | Description |
|---|---|---|
| segment_id | Uuid | Unique identifier |
| filename | String | `{segment_id}.seg` |
| doc_count | u32 | Number of documents in this segment |
| gram_count | u32 | Number of unique gram entries in dictionary |
| created_at | u64 | Unix timestamp |
| base_commit | Option\<String\> | Git commit SHA this segment was built from |

**Internal structure** (on disk):

```
Header {
    magic: [u8; 4],         // b"RPLX"
    version: u32,           // format version
    doc_count: u32,
    gram_count: u32,
    postings_offset: u64,   // byte offset to postings section start
    dict_offset: u64,       // byte offset to dictionary section start
    doc_table_offset: u64,  // byte offset to document metadata table
    toc_offset: u64,        // byte offset to TOC footer
}
```

**Relationships**: A segment contains 0..N documents. A segment is referenced by exactly one manifest (or none, if orphaned/pending GC).

**State transitions**: `Building` -> `Complete` -> `InMerge` -> `Merged` (then deleted).

---

### Dictionary Entry

One entry in the segment's sorted dictionary. Fixed-size for binary search.

| Field | Type | Description |
|---|---|---|
| gram_hash | u64 | Hash of the sparse n-gram |
| postings_offset | u64 | Byte offset into the postings section |
| postings_length | u32 | Number of entries in the posting list |

**Layout**: Packed as 20 bytes per entry, sorted by gram_hash. Page-aligned start for mmap efficiency. Queried via binary search on gram_hash.

---

### Posting List

Sorted list of document IDs associated with a gram.

**Small/medium lists** (doc_count < 8192):

| Field | Type | Description |
|---|---|---|
| encoding | u8 | 0 = delta-varint |
| entries | [u8] | Delta-encoded, varint-compressed doc_ids |

**Large lists** (doc_count >= 8192):

| Field | Type | Description |
|---|---|---|
| encoding | u8 | 1 = roaring |
| data | [u8] | Serialized RoaringBitmap |

**Validation**: Entries must be sorted. Delta values must be positive (no duplicates within a single posting list).

---

### IndexState

Top-level mutable state coordinating base segments, overlay, and snapshot.

| Field | Type | Description |
|---|---|---|
| snapshot | ArcSwap\<IndexSnapshot\> | Atomically swappable, readers clone Arc for stable view |
| pending | Mutex\<Vec\<FileEdit\>\> | Buffered edits, NOT visible to queries |
| on_disk_gens | Vec\<PathBuf\> | On-disk overlay generation files (crash recovery) |
| gitignore | Gitignore | Compiled .gitignore rules for filtering notify_change() |

**IndexSnapshot** (immutable once created):

| Field | Type | Description |
|---|---|---|
| base_segments | Vec\<MmapSegment\> | Immutable base segments (mmap'd) |
| merged_overlay | OverlayView | Single logical overlay (merged from all dirty files) |
| delete_set | RoaringBitmap | Union of all base doc_ids invalidated by overlays |
| path_index | PathIndex | Component-based path/type index |

**FileEdit**:

| Field | Type | Description |
|---|---|---|
| path | String | File path |
| kind | EditKind | Added, Modified, or Deleted |

**Invariant**: `pending` edits are invisible to all queries. `commit_batch()` builds a new `IndexSnapshot` with a rebuilt `OverlayView` and atomically swaps the `ArcSwap`. In-flight searches on the old snapshot are unaffected.

---

### OverlayView

Single merged in-memory view of all dirty files. Rebuilt from scratch on each `commit_batch()`.

| Field | Type | Description |
|---|---|---|
| gram_index | HashMap\<u64, Vec\<u32\>\> | Merged gram hash -> overlay doc_ids |
| docs | Vec\<OverlayDoc\> | All dirty files with current content |
| next_doc_id | u32 | Next overlay-space doc_id |

**OverlayDoc**:

| Field | Type | Description |
|---|---|---|
| doc_id | u32 | Overlay-local doc_id (disjoint from base: base uses 0..N, overlay uses N+1..) |
| path | String | File path |
| content | Vec\<u8\> | File content (kept for verification) |

**Rebuild cost**: for 100 dirty files averaging 20KB, sparse gram extraction + hashmap insertion takes ~5-20ms. This is done on every `commit_batch()` because rebuilding from all dirty files is cheaper than maintaining multiple overlay generations at query time.

**On-disk durability**: each `commit_batch()` also writes a generation file to `overlays/gen_{N}.seg` for crash recovery. These are collapsed into a single file by a background thread. The on-disk files are only used on startup; during normal operation, the in-memory `OverlayView` is authoritative.

**Query execution**: always two logical lookups (base segments + single merged overlay), not N+1.
1. Intersect grams in base segments -> candidate base doc_ids.
2. Subtract `delete_set` (Roaring AND-NOT).
3. Intersect grams in `gram_index` -> candidate overlay doc_ids.
4. Union base and overlay candidates.
5. Verify against file content (overlay docs for dirty files, disk read for base files).

**Full reindex trigger**: merged overlay covers >30% of base file count, or `git checkout`/branch switch. Full reindex is the only mechanism that cleans stale doc_ids from base posting lists.

---

### Manifest

Tracks the set of active base segments and overlay state on disk. Used for crash recovery and index loading.

| Field | Type | Description |
|---|---|---|
| version | u32 | Manifest format version |
| base_commit | Option\<String\> | Git HEAD when base segments were built |
| segments | Vec\<SegmentRef\> | Active base segment references |
| overlay_gen | u64 | Latest overlay generation number |
| overlay_file | Option\<String\> | Consolidated on-disk overlay segment (for crash recovery) |
| overlay_deletes_file | Option\<String\> | On-disk deletion bitmap |
| total_files_indexed | u32 | Total file count (base + overlay) |
| created_at | u64 | Unix timestamp |
| opstamp | u64 | Monotonic operation counter |

**SegmentRef**:

| Field | Type | Description |
|---|---|---|
| segment_id | Uuid | Segment identifier |
| filename | String | Segment file name |
| doc_count | u32 | Document count in this segment |
| gram_count | u32 | Gram count in this segment |

**Persistence**: JSON file, updated via atomic write-then-rename. GC deletes any segment files not referenced by the current manifest.

**On startup**: load base segments from manifest, then rebuild `OverlayView` from the on-disk overlay file (if present). If the overlay file is missing or corrupt, dirty files are re-detected from git diff against `base_commit`.

---

### GramQuery

Internal query representation produced by regex decomposition.

| Variant | Fields | Description |
|---|---|---|
| And | Vec\<GramQuery\> | All sub-queries must match (intersection) |
| Or | Vec\<GramQuery\> | Any sub-query may match (union) |
| Grams | Vec\<u64\> | Gram hashes from a literal, implicitly AND |
| All | (none) | Matches everything, fallback to full scan |
| None | (none) | Matches nothing |

**Simplification rules**:
- `And([..., All, ...])` -> `And([...])` (remove All from AND)
- `Or([..., All, ...])` -> `All` (All in OR dominates)
- `And([])` -> `All`
- `Or([])` -> `None`
- `And([x])` -> `x`, `Or([x])` -> `x`

---

### QueryExecution

Direct execution of a GramQuery tree against the index. No intermediate stack VM. The GramQuery tree IS the execution plan: `And` nodes intersect, `Or` nodes union, `Grams` nodes load and intersect posting lists sorted by ascending cardinality with early termination.

**Execution pseudocode**:
```
execute(And(children)):
    results = execute(children[0])
    for child in children[1..]:
        if results.is_empty(): return empty  // early termination
        results = results.intersect(execute(child))
    return results

execute(Or(children)):
    return union(children.map(execute))

execute(Grams(hashes)):
    sorted_by_cardinality = sort hashes by posting_list_length(hash)
    result = load_postings(sorted[0])
    for hash in sorted[1..]:
        if result.is_empty(): return empty  // early termination
        result = result.intersect(load_postings(hash))
    return result

execute(All): return all_doc_ids
execute(None): return empty
```

**Path filter**: applied as a Roaring bitmap AND before or after gram execution (whichever is cheaper based on estimated sizes). Typically applied first since it is nearly free.

**Verification**: runs after execution produces a candidate set. Not part of the query tree.

---

### PathIndex

Component-based file set index for fast path/type filtering.

| Field | Type | Description |
|---|---|---|
| paths | Vec\<String\> | All file paths, sorted lexicographically. file_id = position. |
| extension_to_files | HashMap\<String, RoaringBitmap\> | Extension (e.g., "rs") -> file_id set |
| component_to_files | HashMap\<String, RoaringBitmap\> | Path component (e.g., "api") -> file_id set |

**Query**: a path filter like `src/api/*.rs` intersects the component bitmaps for "src", "api" and the extension bitmap for "rs". Result: a RoaringBitmap of matching file_ids. Cost: negligible (bitwise AND).

**Language classification**: extension-based mapping stored here. Used by symbol index tier selection and CLI `-t`/`-T` flags.

---

### QueryRoute

Determines which execution path to use for a query.

| Variant | Condition | Execution path |
|---|---|---|
| Literal | Pattern has no regex metacharacters | Sparse gram extraction -> posting intersection -> memchr::memmem verification |
| IndexedRegex | HIR walker extracts >= 1 gram | GramQuery tree -> posting intersection -> regex verification |
| FullScan | No extractable grams (e.g., `.*`) | Path filter (if present) -> scan all matching files with regex |
| SymbolSearch | Query starts with `sym:` or `def:` or `ref:` | SQLite symbol index query -> file lookup |

**Path filter**: applied as the first step in all routes when present. Produces a file_id bitmap that all subsequent operations are intersected with.

---

### SearchResult

| Field | Type | Description |
|---|---|---|
| path | String | File path relative to repo root |
| line_number | u32 | 1-based line number of match |
| line_content | String | The matching line |
| byte_offset | u64 | Byte offset of match start in file |

---

## Entity Relationships

```
Manifest 1--* SegmentRef
SegmentRef 1--1 Segment (on disk, mmap'd)
Segment 1--* Document
Segment 1--* DictionaryEntry
DictionaryEntry 1--1 PostingList

IndexState 1--1 ArcSwap<IndexSnapshot>
IndexState 1--1 Mutex<Vec<FileEdit>> (pending, invisible)

IndexSnapshot 1--* MmapSegment (base)
IndexSnapshot 1--1 OverlayView (single merged overlay)
IndexSnapshot 1--1 RoaringBitmap (delete_set)
IndexSnapshot 1--1 PathIndex

OverlayView 1--* OverlayDoc
OverlayView 1--1 HashMap<u64, Vec<u32>> (gram_index)

PathIndex 1--* RoaringBitmap (per component/extension)

QueryRoute -> GramQuery tree -> [Direct execution with early termination] -> [Verification] -> SearchResult
```

## Validation Rules

1. **Document paths**: Must be valid UTF-8, relative (no leading `/`), no `..` components, no null bytes. Must pass `.gitignore` check.
2. **Segment files**: Must start with magic bytes `b"RPLX"` and valid version. Reject/rebuild on mismatch.
3. **Posting lists**: Must be sorted, no duplicate doc_ids within a list, all doc_ids < segment.doc_count.
4. **Dictionary**: Must be sorted by gram_hash. No duplicate gram_hash entries within a segment.
5. **Manifest**: segment filenames must exist on disk. Orphan files (not in manifest) are candidates for GC.
6. **Overlay doc_ids**: Must not collide with base segment doc_ids (disjoint ranges: base 0..N, overlay N+1..).
7. **Pending edits**: must never be visible to queries. Only `commit_batch()` creates a new IndexSnapshot.
8. **Gram normalization**: all gram hashes are computed from lowercased content, both at index time and query time. Case-sensitive queries produce broader candidate sets (verified by the regex engine).
9. **Post-build assertion**: index size must be logged. Warn if > 0.5x corpus size.
