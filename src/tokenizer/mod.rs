//! Sparse n-gram tokenizer.
//!
//! Extracts variable-length grams from byte sequences using two tiers of
//! boundary detection:
//!
//! 1. **Forced boundaries**: characters that always delimit tokens in source
//!    code (whitespace, punctuation, operators, underscore). These are
//!    context-independent: the same byte always produces a boundary regardless
//!    of surrounding content.
//! 2. **Weight-based boundaries**: within alphanumeric spans, a pre-trained
//!    byte-pair frequency table provides additional subdivision at rare bigrams.
//!
//! # Why forced boundaries exist
//!
//! The original weight-only approach was context-sensitive: boundaries depended
//! on surrounding bytes, so a query's edge grams could differ from the same
//! bytes' grams in a document. Common separators like `space->letter` had
//! weights below `BOUNDARY_THRESHOLD`, causing false negatives. Forced
//! boundaries eliminate this for all token-aligned queries.
//!
//! # Index time: `build_all`
//!
//! Emits hashes of all consecutive-boundary spans with length >= `MIN_GRAM_LEN`.
//! Spans shorter than `MIN_GRAM_LEN` are omitted (no gram can cover them; the
//! verifier handles matches that fall entirely in short spans).
//!
//! # Query time: `build_covering`
//!
//! Greedy left-to-right: emits one gram per consecutive-boundary span that is
//! >= `MIN_GRAM_LEN`. Returns `None` if no such spans exist (full scan required).

/// Weight table for bigram frequencies.
pub mod weights;

use weights::BIGRAM_WEIGHTS;
use xxhash_rust::xxh64::xxh64;

/// Bigram weight threshold. Bigrams with weight >= this are gram boundaries.
///
/// Calibrated against the trained weight table so that underscore-separated
/// code identifiers (snake_case) produce natural gram splits, while common
/// code keywords ("function", "return", "import") remain as single grams.
/// At 28000: `'_'→'q'` (30797) is a boundary, but `'q'→'u'` (17728) is not,
/// so "parse_query" → ["parse_", "query"] rather than one long gram.
///
/// Tune this value if gram quality is poor (too many short grams → lower it;
/// too many long grams → raise it).
pub const BOUNDARY_THRESHOLD: u16 = 28000;

/// Minimum gram length in bytes. Shorter spans are not indexed.
pub const MIN_GRAM_LEN: usize = 3;

/// Maximum gram length in bytes. Longer spans are truncated to the next
/// internal boundary before this limit.
pub const MAX_GRAM_LEN: usize = 128;

// ---------------------------------------------------------------------------
// T014: Gram hash
// ---------------------------------------------------------------------------

/// Hash a gram (variable-length byte slice) to a u64 key for dictionary lookup.
///
/// Uses xxhash64 with seed 0. Same seed must be used at both index and query
/// time.
#[inline]
pub fn gram_hash(gram: &[u8]) -> u64 {
    xxh64(gram, 0)
}

// ---------------------------------------------------------------------------
// Internal boundary detection (two-tier: forced + weight-based)
// ---------------------------------------------------------------------------

/// Characters that always create gram boundaries regardless of bigram weight.
/// These are the natural token delimiters in source code across all major
/// languages. Forced boundaries are context-independent: the same byte always
/// produces a boundary, so query grams match document grams.
#[inline]
fn is_forced_boundary(byte: u8) -> bool {
    matches!(
        byte,
        // Whitespace
        b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C
        // Brackets and grouping
        | b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'<' | b'>'
        // Statement/expression punctuation
        | b'.' | b',' | b':' | b';'
        // Operators
        | b'=' | b'+' | b'-' | b'*' | b'/' | b'%'
        | b'!' | b'&' | b'|' | b'^' | b'~'
        // String/char delimiters
        | b'"' | b'\'' | b'`'
        // Sigils and separators
        | b'@' | b'#' | b'$' | b'?'
        // Underscore: critical for snake_case identifiers
        | b'_'
        // Control characters
        | 0x00..=0x08 | 0x0E..=0x1F | 0x7F
    )
}

/// Returns the list of boundary positions in `bytes`.
///
/// Position 0 and `bytes.len()` are always included. Interior positions use
/// two-tier detection:
/// 1. Forced: either side of position `i` is a delimiter byte.
/// 2. Camel-case: a lowercase ASCII letter followed by uppercase ASCII.
/// 3. Weight-based: `BIGRAM_WEIGHTS[lower(bytes[i-1])*256 + lower(bytes[i])] >= BOUNDARY_THRESHOLD`.
fn boundary_positions(bytes: &[u8]) -> Vec<usize> {
    let n = bytes.len();
    let mut positions = Vec::with_capacity(n / 4);
    positions.push(0);

    for i in 1..n {
        // Tier 1: forced boundary if either adjacent byte is a delimiter
        if is_forced_boundary(bytes[i]) || is_forced_boundary(bytes[i - 1]) {
            positions.push(i);
            continue;
        }
        // Tier 2: lowercase -> uppercase transition in CamelCase identifiers.
        if bytes[i - 1].is_ascii_lowercase() && bytes[i].is_ascii_uppercase() {
            positions.push(i);
            continue;
        }
        // Tier 3: weight-based boundary for rare bigrams within alphanumeric spans
        let left = bytes[i - 1].to_ascii_lowercase();
        let right = bytes[i].to_ascii_lowercase();
        let idx = (left as usize) << 8 | (right as usize);
        if BIGRAM_WEIGHTS[idx] >= BOUNDARY_THRESHOLD {
            positions.push(i);
        }
    }

    if n > 0 {
        positions.push(n);
    }
    // Forced + weight could double-trigger at the same position
    positions.dedup();
    positions
}

/// Like `boundary_positions` but skips inner `to_ascii_lowercase()` since
/// the caller guarantees `bytes` is already lowercase.
#[cfg(test)]
fn boundary_positions_lower(bytes: &[u8]) -> Vec<usize> {
    let n = bytes.len();
    let mut positions = Vec::with_capacity(n / 4);
    positions.push(0);
    for i in 1..n {
        if is_forced_boundary(bytes[i]) || is_forced_boundary(bytes[i - 1]) {
            positions.push(i);
            continue;
        }
        // CamelCase tier skipped: already-lowercase input has no uppercase bytes.
        let idx = (bytes[i - 1] as usize) << 8 | (bytes[i] as usize);
        if BIGRAM_WEIGHTS[idx] >= BOUNDARY_THRESHOLD {
            positions.push(i);
        }
    }
    if n > 0 {
        positions.push(n);
    }
    positions.dedup();
    positions
}

/// Thread-local buffered variant of `boundary_positions_lower`.
/// Reuses a Vec allocation across calls to reduce allocator pressure.
fn boundary_positions_lower_buffered(bytes: &[u8]) -> Vec<usize> {
    thread_local! {
        static BUF: std::cell::RefCell<Vec<usize>> = std::cell::RefCell::new(Vec::with_capacity(256));
    }
    BUF.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        // Shrink the buffer when its capacity far exceeds the current input.
        // This bounds per-thread memory in rayon workers that process large
        // files early in a batch and small files afterward.
        const MIN_CAPACITY: usize = 256;
        let needed = bytes.len() / 4 + 16;
        if buf.capacity() > MIN_CAPACITY.max(needed * 4) {
            buf.shrink_to(MIN_CAPACITY.max(needed));
        }
        let n = bytes.len();
        buf.push(0);
        for i in 1..n {
            if is_forced_boundary(bytes[i]) || is_forced_boundary(bytes[i - 1]) {
                buf.push(i);
                continue;
            }
            let idx = (bytes[i - 1] as usize) << 8 | (bytes[i] as usize);
            if BIGRAM_WEIGHTS[idx] >= BOUNDARY_THRESHOLD {
                buf.push(i);
            }
        }
        if n > 0 {
            buf.push(n);
        }
        buf.dedup();
        buf.clone()
    })
}

// ---------------------------------------------------------------------------
// T012: build_all -- index-time gram extraction
// ---------------------------------------------------------------------------

/// Extract all sparse n-grams from `input` for index construction.
///
/// Finds all boundary positions from the original bytes, lowercases the spans,
/// and emits the hash of each consecutive-boundary span with length in
/// `[MIN_GRAM_LEN, MAX_GRAM_LEN]`.
///
/// Returns an unordered list of hashes. Duplicates are possible and should
/// be deduplicated by the caller (e.g. into a `HashSet` before writing to
/// the segment dictionary).
///
/// # Example
///
/// ```
/// let grams = ripline_rs::tokenizer::build_all(b"parse_query");
/// // Forced boundary at '_' splits into "parse" and "query".
/// assert!(!grams.is_empty());
/// ```
pub fn build_all(input: &[u8]) -> Vec<u64> {
    if input.is_empty() {
        return Vec::new();
    }

    let lower: Vec<u8> = input.iter().map(|b| b.to_ascii_lowercase()).collect();
    let lower_boundaries = boundary_positions_lower_buffered(&lower);
    let mut hashes = Vec::new();
    append_grams_for_boundaries(&mut hashes, &lower, &lower_boundaries);

    // Preserve the token-aligned lowercase spans, then add only the extra
    // spans unlocked by lowercase->uppercase transitions in CamelCase tokens.
    let camel_boundaries = camel_case_boundaries(input);
    if camel_boundaries.is_empty() {
        return hashes;
    }

    let merged_boundaries = merge_boundaries(&lower_boundaries, &camel_boundaries);
    append_new_grams_for_boundaries(&mut hashes, &lower, &lower_boundaries, &merged_boundaries);

    hashes
}

// ---------------------------------------------------------------------------
// T013: build_covering -- query-time covering set extraction
// ---------------------------------------------------------------------------

/// Extract the minimal covering set of grams from a query pattern.
///
/// Lowercases `input`, detects the same boundary positions as the original
/// token-aligned query path, and emits one gram hash per consecutive-boundary span with length >=
/// `MIN_GRAM_LEN`. The result is used as an AND query: all emitted grams
/// must appear in a document for it to be a candidate.
///
/// Returns `None` if no grams of sufficient length exist (the entire query
/// falls in sub-`MIN_GRAM_LEN` spans). Callers must fall back to full scan.
///
/// # Example
///
/// ```
/// use ripline_rs::tokenizer::build_covering;
///
/// // "parse_query" splits at forced boundaries around '_' into
/// // "parse" and "query" (two grams, each >= MIN_GRAM_LEN).
/// let covering = build_covering(b"parse_query").unwrap();
/// assert!(covering.len() >= 2);
///
/// // Short query: no qualifying grams
/// assert!(build_covering(b"ab").is_none());
/// ```
pub fn build_covering(input: &[u8]) -> Option<Vec<u64>> {
    if input.len() < MIN_GRAM_LEN {
        return None;
    }

    let lower: Vec<u8> = input.iter().map(|b| b.to_ascii_lowercase()).collect();
    let boundaries = boundary_positions_lower_buffered(&lower);

    let mut hashes = Vec::new();
    for w in boundaries.windows(2) {
        let (start, end) = (w[0], w[1]);
        let span = end - start;
        if (MIN_GRAM_LEN..=MAX_GRAM_LEN).contains(&span) {
            hashes.push(gram_hash(&lower[start..end]));
        }
        // Spans outside [MIN_GRAM_LEN, MAX_GRAM_LEN] are not covered.
        // This leaves a gap in coverage (more false positives), but correctness
        // is maintained because the verifier always re-checks each candidate.
    }

    if hashes.is_empty() {
        None
    } else {
        Some(hashes)
    }
}

// ---------------------------------------------------------------------------
// T016: build_covering_inner -- regex-safe gram extraction
// ---------------------------------------------------------------------------

/// Extract covering grams from a regex literal fragment.
///
/// Unlike `build_covering` (which treats position 0 and `len` as boundaries),
/// this function refuses spans that rely on synthetic fragment edges. Interior
/// boundaries are safe, because the current tokenizer's boundary decisions are
/// determined by the adjacent bytes at that position.
///
/// For a regex like `parse_quer[yi]`, the HIR literal "parse_quer" ends
/// mid-token. `build_covering` would emit gram "quer" (ending at synthetic
/// `len` boundary), but "quer" is not a gram in documents where the full
/// token is "query". `build_covering_inner` detects that 'r' (the last byte)
/// is not a forced boundary character and skips the partial span.
///
/// Returns `None` if no interior forced-boundary grams exist (caller should
/// fall back to full scan).
pub fn build_covering_inner(input: &[u8]) -> Option<Vec<u64>> {
    if input.len() < MIN_GRAM_LEN {
        return None;
    }

    let lower: Vec<u8> = input.iter().map(|b| b.to_ascii_lowercase()).collect();
    let boundaries = boundary_positions(input);

    let mut hashes = Vec::new();
    for w in boundaries.windows(2) {
        let (start, end) = (w[0], w[1]);
        let span = end - start;
        if !(MIN_GRAM_LEN..=MAX_GRAM_LEN).contains(&span) {
            continue;
        }

        let start_is_real = start > 0 || is_forced_boundary(lower[0]);
        let end_is_real = end < lower.len() || is_forced_boundary(lower[lower.len() - 1]);

        if start_is_real && end_is_real {
            hashes.push(gram_hash(&lower[start..end]));
        }
    }

    if hashes.is_empty() {
        None
    } else {
        Some(hashes)
    }
}

fn append_grams_for_boundaries(hashes: &mut Vec<u64>, lower: &[u8], boundaries: &[usize]) {
    for w in boundaries.windows(2) {
        let (start, end) = (w[0], w[1]);
        let span = end - start;
        if (MIN_GRAM_LEN..=MAX_GRAM_LEN).contains(&span) {
            hashes.push(gram_hash(&lower[start..end]));
        }
        // Spans shorter than MIN_GRAM_LEN: no gram emitted. Queries whose
        // covering set falls entirely in such a span will fall back to full scan.
        // Spans longer than MAX_GRAM_LEN: skipped (very long tokens are not
        // selective and waste posting list space). The verifier handles matches.
    }
}

fn camel_case_boundaries(bytes: &[u8]) -> Vec<usize> {
    let mut positions = Vec::new();
    for i in 1..bytes.len() {
        if bytes[i - 1].is_ascii_lowercase() && bytes[i].is_ascii_uppercase() {
            positions.push(i);
        }
    }
    positions
}

fn merge_boundaries(base: &[usize], extra: &[usize]) -> Vec<usize> {
    let mut merged = Vec::with_capacity(base.len() + extra.len());
    let mut base_i = 0;
    let mut extra_i = 0;

    while base_i < base.len() || extra_i < extra.len() {
        let next = match (base.get(base_i), extra.get(extra_i)) {
            (Some(&base_pos), Some(&extra_pos)) if base_pos <= extra_pos => {
                base_i += 1;
                if base_pos == extra_pos {
                    extra_i += 1;
                }
                base_pos
            }
            (Some(&_base_pos), Some(&extra_pos)) => {
                extra_i += 1;
                extra_pos
            }
            (Some(&base_pos), None) => {
                base_i += 1;
                base_pos
            }
            (None, Some(&extra_pos)) => {
                extra_i += 1;
                extra_pos
            }
            (None, None) => break,
        };

        if merged.last().copied() != Some(next) {
            merged.push(next);
        }
    }

    merged
}

fn append_new_grams_for_boundaries(
    hashes: &mut Vec<u64>,
    lower: &[u8],
    base_boundaries: &[usize],
    merged_boundaries: &[usize],
) {
    let mut base_windows = base_boundaries.windows(2);
    let mut current_base = base_windows.next();

    for merged in merged_boundaries.windows(2) {
        let merged_pair = (merged[0], merged[1]);
        while let Some(base) = current_base {
            let base_pair = (base[0], base[1]);
            if base_pair < merged_pair {
                current_base = base_windows.next();
            } else {
                break;
            }
        }

        if current_base
            .map(|base| (base[0], base[1]) == merged_pair)
            .unwrap_or(false)
        {
            continue;
        }

        let span = merged_pair.1 - merged_pair.0;
        if (MIN_GRAM_LEN..=MAX_GRAM_LEN).contains(&span) {
            hashes.push(gram_hash(&lower[merged_pair.0..merged_pair.1]));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (T015 live in tests/unit/tokenizer.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty() {
        assert!(build_all(b"").is_empty());
        assert!(build_covering(b"").is_none());
    }

    #[test]
    fn short_input_below_min_gram_len() {
        // "ab" is 2 bytes, below MIN_GRAM_LEN=3
        assert!(build_covering(b"ab").is_none());
        // build_all on "ab": one boundary-to-boundary span of length 2, below min
        let grams = build_all(b"ab");
        assert!(grams.is_empty());
    }

    #[test]
    fn lowercase_normalization() {
        // "PARSE" and "parse" must produce the same gram hashes
        let upper = build_all(b"PARSE_QUERY");
        let lower = build_all(b"parse_query");
        assert_eq!(
            upper, lower,
            "uppercase and lowercase must produce same grams"
        );
    }

    #[test]
    fn build_covering_inner_rejects_truncated_fragment_edge() {
        assert!(
            build_covering_inner(b"parse_quer").is_none(),
            "truncated regex literal fragments must not rely on synthetic end boundaries"
        );
    }

    #[test]
    fn build_all_indexes_camel_case_identifiers() {
        let grams = build_all(b"LanguageServerId");
        assert!(
            grams.len() >= 2,
            "camel-case identifiers should contribute extra indexed grams"
        );
    }

    #[test]
    fn build_all_skips_redundant_second_pass_without_camel_case() {
        let input = b"parse_query";
        let lower: Vec<u8> = input.iter().map(|b| b.to_ascii_lowercase()).collect();
        let boundaries = boundary_positions(&lower);
        let mut expected = Vec::new();
        append_grams_for_boundaries(&mut expected, &lower, &boundaries);

        assert_eq!(
            build_all(input),
            expected,
            "non-camel inputs should not pay for a duplicate case-aware pass"
        );
    }

    #[test]
    fn covering_hashes_subset_of_all_hashes() {
        // Every gram in build_covering(s) must also appear in build_all(s).
        // This is the core no-false-negative invariant for the self-contained case.
        let input = b"parse_query";
        let all: std::collections::HashSet<u64> = build_all(input).into_iter().collect();
        let covering = build_covering(input).unwrap_or_default();
        for h in covering {
            assert!(
                all.contains(&h),
                "gram from build_covering not found in build_all on same input"
            );
        }
    }

    #[test]
    fn gram_hash_is_deterministic() {
        let h1 = gram_hash(b"hello");
        let h2 = gram_hash(b"hello");
        assert_eq!(h1, h2);
    }

    #[test]
    fn gram_hash_distinct_for_distinct_grams() {
        let h1 = gram_hash(b"parse");
        let h2 = gram_hash(b"query");
        assert_ne!(h1, h2);
    }

    #[test]
    fn all_same_char_does_not_panic() {
        // "aaa" is 3 bytes, all same. Boundary between identical chars:
        // BIGRAM_WEIGHTS['a','a'] is likely low (common pair), so only boundaries
        // at 0 and 3. Span = 3 = MIN_GRAM_LEN, so one gram emitted.
        let grams = build_all(b"aaa");
        // Either 0 or 1 gram depending on weight; must not panic.
        let _ = grams;
    }

    #[test]
    fn single_byte_does_not_panic() {
        let _ = build_all(b"x");
        assert!(build_covering(b"x").is_none());
    }

    #[test]
    fn boundary_positions_lower_matches_standard_for_lowercase_input() {
        let lower = b"parse_query_and_build";
        assert_eq!(boundary_positions(lower), boundary_positions_lower(lower));
        assert_eq!(
            boundary_positions_lower(lower),
            boundary_positions_lower_buffered(lower)
        );
    }

    #[test]
    fn buffered_boundary_same_as_lower_on_shrink_path() {
        let large_lower: Vec<u8> = (0u8..=127u8).cycle().take(8192)
            .map(|b| b.to_ascii_lowercase()).collect();
        let _ = boundary_positions_lower_buffered(&large_lower);

        let small_lower: Vec<u8> = b"fn foo".iter().map(|b| b.to_ascii_lowercase()).collect();
        let from_lower = boundary_positions_lower(&small_lower);
        let from_buffered = boundary_positions_lower_buffered(&small_lower);
        assert_eq!(from_lower, from_buffered,
            "buffered variant must produce same boundaries as non-buffered after shrink");
    }

    #[test]
    fn long_token_without_internal_boundaries() {
        // A 200-byte string of 'a' with no forced or weight-based internal
        // boundaries: only one span [0,200]. That exceeds MAX_GRAM_LEN=128,
        // so no gram is emitted. build_covering also returns None.
        let input = vec![b'a'; 200];
        let grams = build_all(&input);
        assert!(grams.is_empty());
        assert!(build_covering(&input).is_none());
    }
}
