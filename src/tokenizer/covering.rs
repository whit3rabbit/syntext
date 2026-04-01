//! Query-time covering set extraction.
//!
//! `build_covering` and `build_covering_inner` produce the minimal set of gram
//! hashes needed to query the index for a literal or regex fragment. Moved here
//! from the parent module to keep `mod.rs` under the 400-line limit.

use super::{
    boundary_positions, gram_hash, is_forced_boundary, with_boundary_positions_lower, MAX_GRAM_LEN,
    MIN_GRAM_LEN,
};

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
/// use syntext::tokenizer::build_covering;
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

    // Perf note: the boundary buffer inside with_boundary_positions_lower is a
    // thread-local Vec reused across calls (no per-call allocation). The small
    // `lower` and `hashes` Vecs below are the only allocations, and both callers
    // (route_query, literal_grams) consume the gram data in the Some branch.
    // The early return above already handles the common "too short" case with
    // zero work, so a separate "is indexable?" fast path would only help the
    // rare case where the pattern is long enough but no spans qualify.
    let lower: Vec<u8> = input.iter().map(|b| b.to_ascii_lowercase()).collect();
    with_boundary_positions_lower(&lower, |boundaries| {
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
    })
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
