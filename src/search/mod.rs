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

pub mod verifier;

use std::path::Path;
use std::sync::Arc;

use regex::RegexBuilder;

use crate::index::IndexSnapshot;
use crate::path::filter::build_filter;
use crate::posting::merge_intersect;
use crate::query::{literal_grams, route_query, GramQuery, QueryRoute};
use crate::{Config, IndexError, SearchMatch, SearchOptions};

use verifier::{verify_literal, verify_regex};

/// Run a search against the given snapshot.
///
/// Called by `Index::search()`. Returns matches sorted by path, then line number.
pub fn search(
    snap: Arc<IndexSnapshot>,
    config: &Config,
    pattern: &str,
    opts: &SearchOptions,
) -> Result<Vec<SearchMatch>, IndexError> {
    let route = route_query(pattern, opts.case_insensitive)
        .map_err(IndexError::InvalidPattern)?;

    let compiled_re = match &route {
        QueryRoute::Literal => None,
        _ => {
            let re = RegexBuilder::new(pattern)
                .case_insensitive(opts.case_insensitive)
                .build()
                .map_err(|e| IndexError::InvalidPattern(e.to_string()))?;
            Some(re)
        }
    };

    // Gram-based candidate narrowing. Forced boundaries (whitespace,
    // punctuation, operators, underscore) ensure query grams match document
    // grams for token-aligned queries. FullScan and sub-MIN_GRAM_LEN
    // patterns fall back to all_doc_ids.
    let candidates: Vec<u32> = match &route {
        QueryRoute::Literal => match literal_grams(pattern) {
            Some(ref grams) if should_use_index(grams, &snap) => {
                execute_query(&GramQuery::Grams(grams.clone()), &snap)
            }
            _ => all_doc_ids(&snap),
        },
        QueryRoute::IndexedRegex(gram_query) => execute_query(gram_query, &snap),
        QueryRoute::FullScan => all_doc_ids(&snap),
    };

    // Optional selectivity diagnostics (RIPLINE_LOG_SELECTIVITY=1).
    if std::env::var_os("RIPLINE_LOG_SELECTIVITY").is_some() {
        let total = all_doc_ids(&snap).len();
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

    // Build bitmap-based path/type filter if options are set.
    // Produces a set of allowed file_ids from the PathIndex. Docs not in
    // the PathIndex (overlay docs) fall through to string-based filtering.
    let path_filter_bitmap = build_filter(
        &snap.path_index,
        opts.file_type.as_deref(),
        None, // exclude_type not exposed in SearchOptions yet
        opts.path_filter.as_deref(),
    );

    let mut matches: Vec<SearchMatch> = Vec::new();
    let repo_root = &config.repo_root;

    for global_id in candidates {
        let (rel_path, content) = match resolve_doc(&snap, global_id, repo_root) {
            Some(pair) => pair,
            None => continue,
        };

        // Bitmap-based path/type filtering. If a bitmap filter is active,
        // check file_id membership. Overlay docs (not in PathIndex) fall
        // back to string matching.
        if let Some(ref pf) = path_filter_bitmap {
            if let Some(file_id) = snap.path_index.file_id(&rel_path) {
                if !pf.file_ids.contains(file_id) {
                    continue;
                }
            } else {
                // Overlay doc not in PathIndex: apply string-based fallback.
                if !path_matches_opts(&rel_path, opts) {
                    continue;
                }
            }
        }

        let file_path = Path::new(&rel_path);
        let file_matches = match &route {
            QueryRoute::Literal => verify_literal(pattern, file_path, &content),
            _ => verify_regex(compiled_re.as_ref().unwrap(), file_path, &content),
        };
        matches.extend(file_matches);

        if let Some(max) = opts.max_results {
            if matches.len() >= max {
                matches.truncate(max);
                return Ok(sort_matches(matches));
            }
        }
    }

    Ok(sort_matches(matches))
}

/// String-based path/type filter fallback for docs not in the PathIndex.
fn path_matches_opts(rel_path: &str, opts: &SearchOptions) -> bool {
    if let Some(ref path_filter) = opts.path_filter {
        if !rel_path.contains(path_filter.as_str()) {
            return false;
        }
    }
    if let Some(ref file_type) = opts.file_type {
        let ext = rel_path.rsplit('.').next().unwrap_or("");
        if !ext.eq_ignore_ascii_case(file_type) {
            return false;
        }
    }
    true
}

/// Sort matches by path (lexicographic), then by line number ascending.
fn sort_matches(mut matches: Vec<SearchMatch>) -> Vec<SearchMatch> {
    matches.sort_unstable_by(|a, b| {
        a.path.cmp(&b.path).then_with(|| a.line_number.cmp(&b.line_number))
    });
    matches
}

// ---------------------------------------------------------------------------
// Cardinality-based index bypass
// ---------------------------------------------------------------------------

/// Check whether gram-based narrowing is worth the cost for the given hashes.
///
/// Returns `false` if the smallest posting list exceeds 10% of total docs,
/// since index overhead (dictionary lookups + posting list intersection)
/// outweighs the benefit when selectivity is poor. Uses
/// `MmapSegment::gram_cardinality()` which is O(log n) in the dictionary
/// with no posting list deserialization.
fn should_use_index(hashes: &[u64], snap: &IndexSnapshot) -> bool {
    if hashes.is_empty() {
        return false;
    }

    let total_docs: u32 = snap
        .base_segments()
        .iter()
        .map(|s| s.doc_count)
        .sum::<u32>()
        + snap.overlay.docs.len() as u32;

    if total_docs == 0 {
        return false;
    }

    // The smallest posting list determines the upper bound on intersection
    // size. If even the smallest list is large, full scan is cheaper.
    let min_cardinality = hashes
        .iter()
        .map(|&h| {
            let mut card = 0u32;
            for seg in snap.base_segments() {
                card += seg.gram_cardinality(h).unwrap_or(0);
            }
            if let Some(ids) = snap.overlay.gram_index.get(&h) {
                card += ids.len() as u32;
            }
            card
        })
        .min()
        .unwrap_or(total_docs);

    min_cardinality < total_docs / 10
}

// ---------------------------------------------------------------------------
// GramQuery execution
// ---------------------------------------------------------------------------

/// Execute a `GramQuery` tree against the current snapshot.
///
/// Returns a sorted, deduplicated list of global doc IDs that are candidates
/// for the pattern. False positives are expected; the verifier filters them.
pub fn execute_query(query: &GramQuery, snap: &IndexSnapshot) -> Vec<u32> {
    match query {
        GramQuery::Grams(hashes) => exec_grams(hashes, snap),

        GramQuery::And(children) => {
            if children.is_empty() {
                return all_doc_ids(snap);
            }
            let mut result = execute_query(&children[0], snap);
            for child in &children[1..] {
                if result.is_empty() {
                    return Vec::new();
                }
                let other = execute_query(child, snap);
                result = merge_intersect(&result, &other);
            }
            result
        }

        GramQuery::Or(children) => {
            let mut all: Vec<u32> = Vec::new();
            for child in children {
                let mut part = execute_query(child, snap);
                all.append(&mut part);
            }
            all.sort_unstable();
            all.dedup();
            all
        }

        GramQuery::All => all_doc_ids(snap),
        GramQuery::None => Vec::new(),
    }
}

/// Look up posting lists for each gram hash across all base segments and
/// the overlay, then intersect across hashes.
fn exec_grams(hashes: &[u64], snap: &IndexSnapshot) -> Vec<u32> {
    if hashes.is_empty() {
        return all_doc_ids(snap);
    }

    let mut per_hash: Vec<Vec<u32>> = hashes
        .iter()
        .map(|&hash| {
            let mut docs: Vec<u32> = Vec::new();
            for (seg_idx, seg) in snap.base_segments().iter().enumerate() {
                if let Some(pl) = seg.lookup_gram(hash) {
                    if let Ok(ids) = pl.to_vec() {
                        let base = snap.segment_base_ids().get(seg_idx).copied().unwrap_or(0);
                        docs.extend(ids.iter().map(|&local| base + local));
                    }
                }
            }
            if let Some(ids) = snap.overlay.gram_index.get(&hash) {
                docs.extend_from_slice(ids);
            }
            docs.sort_unstable();
            docs.dedup();
            docs
        })
        .collect();

    per_hash.sort_unstable_by_key(|v| v.len());

    if per_hash[0].is_empty() {
        return Vec::new();
    }
    let mut result = per_hash[0].clone();
    for other in &per_hash[1..] {
        if result.is_empty() {
            return Vec::new();
        }
        result = merge_intersect(&result, other);
    }
    result
}

/// All global doc IDs across base segments + overlay, excluding delete_set.
fn all_doc_ids(snap: &IndexSnapshot) -> Vec<u32> {
    let mut ids = Vec::new();
    for (seg_idx, seg) in snap.base_segments().iter().enumerate() {
        let base = snap.segment_base_ids().get(seg_idx).copied().unwrap_or(0);
        for local in 0..seg.doc_count {
            let global = base + local;
            if !snap.delete_set.contains(global) {
                ids.push(global);
            }
        }
    }
    // Overlay doc_ids.
    for doc in &snap.overlay.docs {
        ids.push(doc.doc_id);
    }
    ids
}

/// Resolve a global doc ID to its path and content.
///
/// Overlay docs return in-memory content (Arc-shared, no copy).
/// Base docs read from disk. Returns `None` if the doc is deleted,
/// out of range, or unreadable.
fn resolve_doc(
    snap: &IndexSnapshot,
    global_id: u32,
    repo_root: &Path,
) -> Option<(String, Arc<[u8]>)> {
    // Check overlay first (overlay doc_ids are >= base_doc_count).
    if let Some(doc) = snap.overlay.get_doc(global_id) {
        return Some((doc.path.clone(), Arc::clone(&doc.content)));
    }

    // Deleted base doc.
    if snap.delete_set.contains(global_id) {
        return None;
    }

    // Base segment lookup.
    if snap.segment_base_ids().is_empty() {
        return None;
    }
    let seg_idx = snap
        .segment_base_ids()
        .partition_point(|&b| b <= global_id)
        .saturating_sub(1);
    if seg_idx >= snap.base_segments().len() {
        return None;
    }
    let base = snap.segment_base_ids()[seg_idx];
    let local_id = global_id.checked_sub(base)?;
    let doc_entry = snap.base_segments()[seg_idx].get_doc(local_id)?;

    let abs_path = repo_root.join(&doc_entry.path);
    let content = std::fs::read(&abs_path).ok()?;
    Some((doc_entry.path, Arc::from(content)))
}
