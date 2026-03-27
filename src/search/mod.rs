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

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;
use regex::RegexBuilder;
use roaring::RoaringBitmap;

use crate::index::IndexSnapshot;
use crate::path::filter::{build_filter, matches_path_filter};
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
    let route = route_query(pattern, opts.case_insensitive).map_err(IndexError::InvalidPattern)?;

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

    let candidates: Vec<u32> = match &route {
        QueryRoute::Literal => match literal_grams(pattern) {
            Some(hashes) if should_use_index(&hashes, &snap) => {
                execute_query(&GramQuery::Grams(hashes), &snap)?
            }
            _ => all_doc_ids(&snap),
        },
        QueryRoute::IndexedRegex(query) => execute_query(query, &snap)?,
        _ => all_doc_ids(&snap),
    };

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
            let (rel_path, content) =
                resolve_doc(&snap, global_id, repo_root, config.max_file_size)?;

            if let Some(ref pf) = path_filter_bitmap {
                let file_id_opt = snap
                    .doc_to_file_id
                    .get(global_id as usize)
                    .copied()
                    .filter(|&fid| fid != u32::MAX);
                if let Some(file_id) = file_id_opt {
                    if !pf.file_ids.contains(file_id) {
                        return None;
                    }
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
        a.path
            .cmp(&b.path)
            .then_with(|| a.line_number.cmp(&b.line_number))
    });
    matches
}

/// All global doc IDs across base segments + overlay, excluding delete_set.
fn all_doc_ids(snap: &IndexSnapshot) -> Vec<u32> {
    snap.all_doc_ids().iter().collect()
}

/// Return true if the smallest literal posting list is selective enough to
/// justify index execution instead of a full scan.
fn should_use_index(hashes: &[u64], snap: &IndexSnapshot) -> bool {
    if hashes.is_empty() {
        return false;
    }

    let total_docs = snap.all_doc_ids().len();
    if total_docs == 0 {
        return false;
    }

    let smallest = hashes
        .iter()
        .map(|&hash| gram_cardinality(hash, snap))
        .min()
        .unwrap_or(0);

    u64::from(smallest) * 10 <= total_docs
}

/// Execute a gram query against base segments plus overlay and return sorted
/// global doc IDs.
fn execute_query(query: &GramQuery, snap: &IndexSnapshot) -> Result<Vec<u32>, IndexError> {
    Ok(execute_query_bitmap(query, snap)?.iter().collect())
}

fn execute_query_bitmap(
    query: &GramQuery,
    snap: &IndexSnapshot,
) -> Result<RoaringBitmap, IndexError> {
    match query {
        GramQuery::And(children) => {
            let mut iter = children.iter();
            let Some(first) = iter.next() else {
                return Ok(snap.all_doc_ids().clone());
            };
            let mut acc = execute_query_bitmap(first, snap)?;
            for child in iter {
                let child_bitmap = execute_query_bitmap(child, snap)?;
                acc &= &child_bitmap;
                if acc.is_empty() {
                    break;
                }
            }
            Ok(acc)
        }
        GramQuery::Or(children) => {
            let mut acc = RoaringBitmap::new();
            for child in children {
                let child_bitmap = execute_query_bitmap(child, snap)?;
                acc |= &child_bitmap;
            }
            Ok(acc)
        }
        GramQuery::Grams(hashes) => {
            let mut iter = hashes.iter().copied();
            let Some(first) = iter.next() else {
                return Ok(snap.all_doc_ids().clone());
            };
            let mut acc = posting_bitmap(first, snap)?;
            for hash in iter {
                let postings = posting_bitmap(hash, snap)?;
                acc &= &postings;
                if acc.is_empty() {
                    break;
                }
            }
            Ok(acc)
        }
        GramQuery::All => Ok(snap.all_doc_ids().clone()),
        GramQuery::None => Ok(RoaringBitmap::new()),
    }
}

fn gram_cardinality(gram_hash: u64, snap: &IndexSnapshot) -> u32 {
    let base_total: u32 = snap
        .base_segments()
        .iter()
        .filter_map(|seg| seg.gram_cardinality(gram_hash))
        .sum();
    let overlay_total = snap
        .overlay
        .gram_index
        .get(&gram_hash)
        .map_or(0, |ids| ids.len() as u32);
    base_total.saturating_add(overlay_total)
}

fn posting_bitmap(gram_hash: u64, snap: &IndexSnapshot) -> Result<RoaringBitmap, IndexError> {
    let mut bitmap = RoaringBitmap::new();

    for seg in snap.base_segments() {
        if let Some(postings) = seg.lookup_gram(gram_hash) {
            let ids = postings
                .to_vec()
                .map_err(|err| IndexError::CorruptIndex(err.to_string()))?;
            bitmap.extend(ids);
        }
    }

    if let Some(ids) = snap.overlay.gram_index.get(&gram_hash) {
        bitmap.extend(ids.iter().copied());
    }

    bitmap -= &snap.delete_set;
    Ok(bitmap)
}

/// Resolve a global doc ID to its path and content.
///
/// Overlay docs return in-memory content (Arc-shared, no copy).
/// Base docs read from disk, capped at `max_file_size` bytes. If a file grew
/// after indexing we still verify against the truncated content rather than
/// skipping it -- skipping would be a false negative because the file was
/// already indexed from its smaller version and candidates were generated.
/// Returns `None` if the doc is deleted, out of range, or unreadable.
fn resolve_doc(
    snap: &IndexSnapshot,
    global_id: u32,
    repo_root: &Path,
    max_file_size: u64,
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
    // Guard: reject symlinks that resolve outside the repo root.
    // canonicalize() resolves all symlinks; if the result doesn't start
    // with repo_root, the file has escaped.
    let canonical = std::fs::canonicalize(&abs_path).ok()?;
    let canonical_root = std::fs::canonicalize(repo_root).ok()?;
    if !canonical.starts_with(&canonical_root) {
        return None;
    }
    // Bounded read: cap at max_file_size to prevent unbounded memory growth
    // when a file grows after it was indexed.
    let file = std::fs::File::open(&canonical).ok()?;
    let mut reader = file.take(max_file_size);
    let mut content = Vec::new();
    reader.read_to_end(&mut content).ok()?;
    Some((doc_entry.path, Arc::from(content)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::index::Index;
    use crate::query::literal_grams;
    use crate::Config;

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

    #[test]
    fn literal_queries_short_circuit_when_grams_are_missing() {
        let index_dir = TempDir::new().unwrap();
        let config = Config {
            index_dir: index_dir.path().to_path_buf(),
            repo_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus"),
            ..Config::default()
        };
        let index = Index::build(config).unwrap();
        let snap = index.snapshot();
        let grams = literal_grams("xyzzy_no_match_sentinel_42").unwrap();

        assert!(should_use_index(&grams, &snap));

        let candidates = execute_query(&GramQuery::Grams(grams), &snap).unwrap();
        assert!(candidates.is_empty());
    }
}
