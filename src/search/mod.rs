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

use rayon::prelude::*;
use regex::RegexBuilder;

use crate::index::IndexSnapshot;
use crate::path::filter::{build_filter, matches_path_filter};
use crate::query::{route_query, QueryRoute};
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

    // Fallback to all documents.
    let candidates: Vec<u32> = all_doc_ids(&snap);

    // Optional selectivity diagnostics (RIPLINE_LOG_SELECTIVITY=1).
    if std::env::var_os("RIPLINE_LOG_SELECTIVITY").is_some() {
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

    // Build bitmap-based path/type filter if options are set.
    // Produces a set of allowed file_ids from the PathIndex. Docs not in
    // the PathIndex (overlay docs) fall through to string-based filtering.
    let path_filter_bitmap = build_filter(
        &snap.path_index,
        opts.file_type.as_deref(),
        None, // exclude_type not exposed in SearchOptions yet
        opts.path_filter.as_deref(),
    );

    let repo_root = &config.repo_root;

    // Parallel resolve + filter + verify. Loses serial early-exit on
    // max_results, but parallel I/O vastly outweighs this for typical
    // workloads (NVMe queue depth exploitation, kernel I/O scheduling).
    let all_matches: Vec<SearchMatch> = candidates
        .par_iter()
        .filter_map(|&global_id| {
            let (rel_path, content) = resolve_doc(&snap, global_id, repo_root)?;

            if let Some(ref pf) = path_filter_bitmap {
                let file_id_opt = snap.doc_to_file_id.get(global_id as usize)
                    .copied()
                    .filter(|&fid| fid != u32::MAX);
                if let Some(file_id) = file_id_opt {
                    if !pf.file_ids.contains(file_id) { return None; }
                } else if !matches_path_filter(
                    &rel_path,
                    opts.file_type.as_deref(),
                    None,
                    opts.path_filter.as_deref(),
                ) {
                    return None;
                }
            }

            let file_path = Path::new(&rel_path);
            let file_matches = match &route {
                QueryRoute::Literal => verify_literal(pattern, file_path, &content),
                _ => verify_regex(compiled_re.as_ref().unwrap(), file_path, &content),
            };
            Some(file_matches)
        })
        .flatten()
        .collect();

    let mut matches = sort_matches(all_matches);
    if let Some(max) = opts.max_results {
        matches.truncate(max);
    }
    Ok(matches)
}

/// Sort matches by path (lexicographic), then by line number ascending.
fn sort_matches(mut matches: Vec<SearchMatch>) -> Vec<SearchMatch> {
    matches.sort_unstable_by(|a, b| {
        a.path.cmp(&b.path).then_with(|| a.line_number.cmp(&b.line_number))
    });
    matches
}



/// All global doc IDs across base segments + overlay, excluding delete_set.
fn all_doc_ids(snap: &IndexSnapshot) -> Vec<u32> {
    snap.all_doc_ids().iter().collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_path_filter_uses_same_glob_semantics() {
        let opts = SearchOptions {
            path_filter: Some("*.rs".to_string()),
            file_type: None,
            max_results: None,
            case_insensitive: false,
        };

        assert!(matches_path_filter(
            "src/main.rs",
            opts.file_type.as_deref(),
            None,
            opts.path_filter.as_deref(),
        ));
        assert!(!matches_path_filter(
            "src/main.py",
            opts.file_type.as_deref(),
            None,
            opts.path_filter.as_deref(),
        ));
    }
}
