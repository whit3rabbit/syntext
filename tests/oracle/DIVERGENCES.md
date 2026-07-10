# st ↔ rg Oracle Divergences Policy

This document enumerates the known, intentional, and documented correctness/semantic differences between `st` (syntext) and `rg` (ripgrep). Mismatches listed here are expected and allowed in the testing harness.

## Known Divergences

### 1. Smart-case HIR Handling
- **Description**: `st` handles smart-case query decomposition by analyzing token boundaries and generating optional case-insensitive grams. Detail-documented in `src/cli/mod.rs`.
- **Harness Action**: Accept mismatch under smart-case queries where the candidate set or verification ranges differ slightly due to token-level boundary constraints.

### 2. Invert-match (`-v`) Candidate-Only Semantics
- **Description**: `st`'s invert-match currently runs under a candidate-only scope filter (documented v1 limitation) which matches files not containing the query, rather than line-by-line inversion across all files.
- **Harness Action**: Bypass line-by-line equality comparison when `-v` is active.

### 3. Result Ordering
- **Description**: `st` sorts results lexicographically by path and then ascending by line number. `rg` outputs matches in walk order (unless `--sort path` is specified, but behavior can vary with parallel worker threads).
- **Harness Action**: Sorting both result lists by path and line number before comparing them.

### 4. Symbol Searches (`sym:`, `def:`, `ref:`)
- **Description**: Special prefixes in `st` trigger AST-based symbol searches. `rg` treats these as literal strings.
- **Harness Action**: Do not run differential tests against patterns starting with `sym:`, `def:`, or `ref:`.

### 5. PCRE2 / Multiline / Unsupported Regex Flags
- **Description**: Flags such as PCRE2, multiline, lookarounds are not supported by `st`'s pure regex engine (which wraps `regex::bytes::Regex`).
- **Harness Action**: Do not generate test cases using unsupported regex features.

### 6. `--vimgrep` Column Numbers for Multibyte Characters
- **Description**: The `--vimgrep` format includes 1-based column byte offsets. For multibyte Unicode sequences, `st` and `rg` may compute column numbers differently depending on how they count partial codepoints. This is an edge case in the rendering layer only (it does not affect match correctness).
- **Harness Action**: When comparing `--vimgrep` output (Tier C), strip the column field from both outputs and compare only `path:line:content`. The column field is treated as a non-comparable cosmetic.

### 7. `-c` / `--count-matches` Line Format
- **Description**: `rg -c` emits `path:count` with the count of *matching lines*; `rg --count-matches` emits `path:count` with the count of *individual match occurrences*. `st -c` follows the same `-c` / `--count-matches` distinction. Results are byte-comparable after path normalization.
- **Harness Action**: Tier C applies — normalize `./` path prefix and compare sorted lines exactly.

### 8. Flag No-ops
- **Description**: Many flags are accepted by `st` for ripgrep CLI compatibility but are no-ops for indexed search (e.g., `--hidden`, `--no-ignore`, `--follow`, `--threads`, `--mmap`, `--color`). Passing these flags to both engines produces identical match content.
- **Harness Action**: Excluded from flag sampling — testing them adds no correctness signal.

## Comparison Invariant Tiers

- **Tier A (Hard)**: Search routing correctness. Every match reported by `rg` *must* appear in the `st` results (no false negatives).
- **Tier B (Goal)**: Complete match set equality. The sorted list of matches `(path, line_number, submatch_start, submatch_end)` is identical between both engines.
- **Tier C (Soft)**: Byte-identical output rendering. The rendered output formats (`--vimgrep`, `-c`, `-l`) match exactly after normalization (path prefix stripping, column field removal for `--vimgrep`). `--json` is covered by Tier A/B via the NDJSON normalizer.


### 9. Wildcard Queries (`.*`) on Non-UTF-8 Files

- **Description**: Queries like `.*` or `.*parse` match virtually every byte position. On UTF-16 or other non-UTF-8 encoded files, `rg` decodes the content and produces character-level matches, while `st` treats such files as non-indexable (binary or non-UTF-8). The match sets differ for these pathological queries.
- **Harness Action**: Queries `.*` and `.*parse` are excluded from the proptest query pool. Token-aligned queries (which represent real search patterns) are used instead.

### 10. Submatch Byte Offset Differences for Broad Alternations

- **Description**: For queries like `a|b|c|...|u`, both `rg` and `st` find the same matched lines, but the reported submatch start/end byte offsets can differ. For example, on a line `.syntext/`, `rg` may report the match starting at byte 0 (matching `.` with a broad alternation) while `st` starts at byte 1 (matching `s`). Both are valid matches at the correct line.
- **Harness Action**: Tier A is enforced at `(path, line_number)` granularity only. Submatch byte offsets are not compared at Tier A — they are a Tier B concern only for exact token-aligned queries where both engines should agree.

### 11. Byte Patterns (`(?-u)\xNN`) with `-w` on Invalid-UTF-8 Content

- **Description**: For a raw byte pattern like `(?-u)\xff\xfe` combined with `-w` (word boundary) against invalid-UTF-8 content (e.g. `foo\xff\xfebar`), ripgrep reports **no match** (exit 1) while `st` matches. `st` verifies with the `regex` crate, whose `\b` recognises the word/non-word transitions on either side of the byte run (`o`→`\xff`, `\xfe`→`b`); rg's `-w` does not match byte-level patterns on a non-UTF-8 haystack. The two engines agree on the same pattern *without* `-w`, and on valid-UTF-8 content with `-w`.
- **Harness Action**: The `(?-u)\xff\xfe` query is excluded from the proptest query pool (`generate_query`), matching the existing exclusion of `.*`/wildcard queries on non-UTF-8 content (see #9). Invalid-UTF-8 files remain in the corpus and are still exercised via token-aligned queries.

### 12. Line-text Rendering of Non-UTF-8 / UTF-16 Content

- **Description**: `st`'s `normalize_encoding` converts UTF-16 / BOM / non-UTF-8 content to UTF-8 at index time, so `st` reports clean UTF-8 line text (e.g. `query`). `rg` decodes the same bytes and renders invalid sequences as U+FFFD (e.g. `query�`). Both engines agree on the match location and matched bytes; only the surrounding line *text* renders differently.
- **Harness Action**: `CanonicalMatch` (Tier A/B) excludes line text — comparison is `(path, line_number, submatches)` per the Tier B contract. Line-text rendering is only checked at Tier C via raw-output comparison (`compare_rendered_output`).
