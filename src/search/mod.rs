//! Search executor: routes queries to the gram index or full scan, then verifies
//! candidates against actual file content.
//!
//! # Execution flow
//!
//! 1. `route_query()` classifies the pattern (Literal / IndexedRegex / FullScan).
//! 2. `execute_query()` walks the `GramQuery` tree against base segments + overlay,
//!    producing a sorted list of candidate global doc IDs.
//! 3. Each candidate doc is read from disk (or overlay memory) and passed to the verifier.
//! 4. Matches are sorted by path, then line number.

mod executor;
pub(crate) mod lines;
mod resolver;
/// Tiered verifier using literal search (memchr) and regex engines.
pub mod verifier;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use resolver::resolve_doc;

#[cfg(feature = "rayon")]
use rayon::prelude::*;
use regex::bytes::RegexBuilder;
use roaring::RoaringBitmap;

use crate::index::IndexSnapshot;
use crate::path::filter::{build_filter, matches_path_filter};
use crate::query::{literal_grams, route_query, GramQuery, QueryRoute};
use crate::{Config, IndexError, SearchMatch, SearchOptions};

use executor::{execute_query, gram_cardinality, is_selective_enough, posting_bitmap};

/// 10 MiB cap on regex NFA/DFA size: prevents ReDoS during compilation (not just matching).
///
/// Security audit: user-supplied patterns pass through two layers before matching:
///   1. `regex_syntax::Parser` in `decompose()` validates HIR structure and rejects
///      invalid syntax before the main `RegexBuilder` sees the pattern.
///   2. `RegexBuilder::size_limit` + `dfa_size_limit` (set here) cap the NFA/DFA
///      automaton size at compilation time, bounding both memory and CPU.
///
/// Together these prevent catastrophic backtracking and unbounded automaton growth
/// from adversarial patterns. The `regex` crate's RE2-style engine guarantees
/// linear-time matching once compiled, so matching itself is not a ReDoS vector.
pub(crate) const REGEX_SIZE_LIMIT: usize = 10 * 1024 * 1024;

use verifier::{verify_literal, verify_regex};

/// Run a search against the given snapshot.
///
/// Called by `Index::search()`. Returns matches sorted by path, then line number.
pub fn search(
    snap: Arc<IndexSnapshot>,
    config: &Config,
    canonical_root: &std::path::Path,
    pattern: &str,
    opts: &SearchOptions,
) -> Result<Vec<SearchMatch>, IndexError> {
    #[cfg(any(test, feature = "oracle"))]
    let route = if opts.force_full_scan {
        QueryRoute::FullScan
    } else {
        route_query(pattern, opts.case_insensitive).map_err(IndexError::InvalidPattern)?
    };
    #[cfg(not(any(test, feature = "oracle")))]
    let route = route_query(pattern, opts.case_insensitive).map_err(IndexError::InvalidPattern)?;

    // When verify_pattern is set (e.g. -w/-x wrapping), use it for
    // verification while the routing pattern (unwrapped) drives gram
    // extraction. This prevents boundary-wrapping from producing
    // zero grams and forcing a full scan.
    let verify_pattern = opts.verify_pattern.as_deref().unwrap_or(pattern);

    // Build the verifier regex for every route except a plain (unwrapped)
    // literal. A wrapped literal (-w/-x) still routes as `Literal` because its
    // grams come from the unwrapped pattern, but it must verify with the
    // boundary-wrapped regex held in `verify_pattern`, so it needs compiled_re.
    let compiled_re = if matches!(route, QueryRoute::Literal) && opts.verify_pattern.is_none() {
        None
    } else {
        let re = RegexBuilder::new(verify_pattern)
            .case_insensitive(opts.case_insensitive)
            .multi_line(true)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(|e| IndexError::InvalidPattern(e.to_string()))?;
        Some(re)
    };

    let candidates: Vec<u32> = match &route {
        QueryRoute::Literal => match literal_grams(pattern) {
            // Required grams are always strictly-interior boundaries that
            // build_all emits for any matching doc, so AND-of-required alone
            // catches every match (no false negatives). Optional (synthetic-
            // edge) grams are deliberately NOT intersected: ANDing them would
            // drop sub-token matches (e.g. literal "parse" missing "reparse").
            // With no required gram the index cannot anchor the pattern, so
            // fall back to full scan and let memchr see every candidate.
            Some(covering) if !covering.required.is_empty() => {
                if should_use_index(&covering.required, &snap)? {
                    execute_query(&GramQuery::Grams(covering.required), &snap)?
                } else {
                    all_doc_ids(&snap)
                }
            }
            _ => all_doc_ids(&snap),
        },
        QueryRoute::IndexedRegex(query) => {
            // A plain gram set (case-insensitive literal) gets the same
            // selectivity probe as the Literal arm, so a common required gram
            // falls back to scan instead of materializing a huge posting list.
            // Decomposed regex trees (non-literal) use the index directly.
            let selective = match query {
                GramQuery::Grams(grams) if !grams.is_empty() => {
                    should_use_index(grams, &snap)?
                }
                _ => true,
            };
            if !selective {
                all_doc_ids(&snap)
            } else {
                let indexed = execute_query(query, &snap)?;
                if indexed.is_empty() {
                    all_doc_ids(&snap)
                } else {
                    indexed
                }
            }
        }
        _ => all_doc_ids(&snap),
    };

    // Optional selectivity diagnostics (SYNTEXT_LOG_SELECTIVITY=1).
    if std::env::var_os("SYNTEXT_LOG_SELECTIVITY").is_some() {
        let total = snap.all_doc_ids().len() as usize;
        let pct = if total > 0 {
            candidates.len() as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        eprintln!(
            "selectivity: {:.2}% ({}/{}) route={:?} pattern={:?}",
            pct,
            candidates.len(),
            total,
            route,
            pattern
        );
    }

    let glob_cache = snap.glob_cache.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let path_filter_bitmap = build_filter(
        &snap.path_index,
        opts.file_type.as_deref(),
        opts.exclude_type.as_deref(),
        opts.path_filter.as_deref(),
        Some(glob_cache),
    );
    // Only built when a path/type filter is actually in play: this is what
    // triggers the (otherwise skipped) lazy base_doc_id -> file_id build.
    let base_doc_to_file_id = path_filter_bitmap
        .as_ref()
        .map(|_| snap.base_doc_to_file_id());

    // Parallel resolve + filter + verify. An atomic counter enables early-exit
    // once max_results is reached: threads skip resolve+verify once the
    // counter reaches the limit. Relaxed ordering is intentional: a few extra
    // files may be processed near the boundary, which is acceptable since the
    // final truncate enforces the hard limit.
    let deterministic = std::env::var("SYNTEXT_DETERMINISTIC")
        .ok()
        .is_some_and(|v| v == "1");
    let match_count = AtomicUsize::new(0);
    let do_match = |&global_id: &u32| -> Option<Vec<SearchMatch>> {
        // Early-exit: skip expensive I/O once we already have enough matches.
        if let Some(limit) = opts.max_results {
            if !deterministic && match_count.load(Ordering::Relaxed) >= limit {
                return None;
            }
        }
        let (rel_path, content) = resolve_doc(
            &snap,
            global_id,
            canonical_root,
            config.max_file_size,
            config.verbose,
        )?;

        if let Some(ref pf) = path_filter_bitmap {
            // Safe to unwrap: base_doc_to_file_id is set above whenever
            // path_filter_bitmap is Some.
            let doc_to_file_id = base_doc_to_file_id.as_ref().expect("built alongside pf");
            let file_id_opt = if (global_id as usize) < doc_to_file_id.len() {
                doc_to_file_id
                    .get(global_id as usize)
                    .copied()
                    .filter(|&fid| fid != u32::MAX)
            } else {
                snap.overlay_doc_to_file_id.get(&global_id).copied()
            };
            if let Some(file_id) = file_id_opt {
                if !pf.file_ids.contains(file_id) {
                    return None;
                }
            } else {
                if !matches_path_filter(
                    &rel_path,
                    opts.file_type.as_deref(),
                    opts.exclude_type.as_deref(),
                    opts.path_filter.as_deref(),
                ) {
                    return None;
                }
            }
        }

        let file_path = rel_path.as_path();
        let file_matches = match &route {
            // Unwrapped literal: fast memchr path. A wrapped literal (-w/-x)
            // falls through to the regex verifier, since verify_pattern holds
            // the boundary-wrapped pattern and memchr would search for its
            // literal bytes (matching nothing).
            QueryRoute::Literal if opts.verify_pattern.is_none() => {
                verify_literal(verify_pattern, file_path, &content, opts.skip_line_content)
            }
            _ => verify_regex(
                compiled_re.as_ref().unwrap(),
                file_path,
                &content,
                opts.skip_line_content,
            ),
        };
        if let Some(_limit) = opts.max_results {
            if !file_matches.is_empty() {
                match_count.fetch_add(file_matches.len(), Ordering::Relaxed);
            }
        }
        Some(file_matches)
    };
    #[cfg(feature = "rayon")]
    let all_matches: Vec<SearchMatch> = candidates
        .par_iter()
        .filter_map(do_match)
        .flatten()
        .collect();
    #[cfg(not(feature = "rayon"))]
    let all_matches: Vec<SearchMatch> = candidates.iter().filter_map(do_match).flatten().collect();

    let mut matches = sort_matches(all_matches);
    if let Some(max) = opts.max_results {
        matches.truncate(max);
    }
    Ok(matches)
}

/// Sort matches by path (lexicographic), then by line number ascending.
fn sort_matches(mut matches: Vec<SearchMatch>) -> Vec<SearchMatch> {
    matches.sort_unstable_by(|a, b| {
        crate::path_util::cmp_path_bytes(&a.path, &b.path)
            .then_with(|| a.line_number.cmp(&b.line_number))
    });
    matches
}

/// All global doc IDs across base segments + overlay, excluding delete_set.
fn all_doc_ids(snap: &IndexSnapshot) -> Vec<u32> {
    snap.all_doc_ids().iter().collect()
}

/// Return true if the literal looks selective enough to justify indexed
/// execution instead of a full scan.
fn should_use_index(hashes: &[u64], snap: &IndexSnapshot) -> Result<bool, IndexError> {
    if hashes.is_empty() {
        return Ok(false);
    }

    let total_docs = snap.all_doc_ids().len();
    if total_docs == 0 {
        return Ok(false);
    }

    let mut ordered = hashes.to_vec();
    ordered.sort_unstable_by_key(|&hash| gram_cardinality(hash, snap));

    let smallest = ordered
        .first()
        .map(|&hash| gram_cardinality(hash, snap))
        .unwrap_or(0);
    if is_selective_enough(u64::from(smallest), total_docs, snap.scan_threshold) {
        return Ok(true);
    }

    if ordered.len() == 1 {
        return Ok(false);
    }

    // Probe the intersection of a few smallest postings. Compound identifiers
    // can be highly selective even when each component gram is common alone.
    // Use & on two borrows to avoid cloning the first (potentially large) bitmap.
    let first = posting_bitmap(ordered[0], snap)?;
    let second = posting_bitmap(ordered[1], snap)?;
    let mut acc: RoaringBitmap = first.as_ref() & second.as_ref();
    if acc.is_empty() || is_selective_enough(acc.len(), total_docs, snap.scan_threshold) {
        return Ok(true);
    }
    if ordered.len() > 2 {
        let third = posting_bitmap(ordered[2], snap)?;
        acc &= third.as_ref();
        if acc.is_empty() || is_selective_enough(acc.len(), total_docs, snap.scan_threshold) {
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests;
