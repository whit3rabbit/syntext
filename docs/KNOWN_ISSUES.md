# Known Issues

## ~~Gram index candidate narrowing is disabled (full-scan fallback)~~ RESOLVED

**Status**: Resolved (forced boundary approach)
**Files**: `src/tokenizer/mod.rs`, `src/search/mod.rs`, `src/query/mod.rs`, `src/query/regex_decompose.rs`

### Problem (was)

The weight-only boundary detection was context-sensitive: boundaries depended on surrounding bytes, so a query's edge grams could differ from the same bytes' grams in a document. Common separators like `space->letter` had weights below `BOUNDARY_THRESHOLD`, causing false negatives. The weight values documented here were stale (e.g., `space->p` was actually 14708, not the documented 26213).

### Resolution: forced boundaries

Two-tier boundary detection replaces the weight-only approach:

1. **Forced boundaries** (Tier 1): whitespace, punctuation, operators, underscore, and control characters always create boundaries regardless of bigram weight. These are context-independent.
2. **Weight-based boundaries** (Tier 2): within alphanumeric spans, the trained weight table provides additional subdivision at rare bigrams (unchanged).

Forced boundaries ensure that token-aligned queries (the 99% case in code search) produce the same grams in both query and document contexts. The `_` character is forced, so `parse_query` reliably splits into `parse` and `query` in all contexts.

**Literal queries** (`QueryRoute::Literal`) use `build_covering`, which treats position 0/len as real boundaries. This is correct because the user is searching for a complete token.

**Regex queries** use `build_covering_inner`, which only emits grams whose both boundaries are at forced-boundary characters (not synthetic 0/len). This avoids false negatives when regex literals end mid-token (e.g., `parse_quer` from `parse_quer[yi]`). When no interior grams exist, the query falls back to full scan.

### Remaining limitations

- **Regex index narrowing**: most regex patterns fall back to full scan because short regex literals lack interior forced-boundary grams. This is correct (no false negatives) and acceptable: literal queries (the majority of agent workflow queries) benefit from narrowing.
- **Mid-token embedding**: a literal query like `parse_query` embedded in `xparse_queryy` (no forced boundaries around it) produces a false negative. This is rare in real code and an accepted tradeoff.
- **CamelCase substrings**: searching for `Map` inside `HashMap` may miss if weight-based boundaries don't split at the right point. Overlapping trigrams within spans (Phase B) would fix this.

### Verification

- All 13 correctness tests pass with zero false negatives against the ripgrep oracle.
- Property-based fuzzing (5,000 proptest cases) confirms the token-aligned covering invariant holds: `covering(Q) subset_of all(D)` for every token-aligned substring Q of every generated document D.
