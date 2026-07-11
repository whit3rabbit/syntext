//! Query execution against base segments + overlay.
//!
//! Evaluates `GramQuery` trees using cardinality-based intersection ordering.
//! An aggregate `PostingBudget` caps total memory materialized per query to
//! prevent OOM from crafted indexes with many large posting lists.

use std::cell::Cell;
use std::sync::Arc;

use roaring::RoaringBitmap;

use crate::index::IndexSnapshot;
use crate::query::GramQuery;
use crate::IndexError;

/// Maximum aggregate posting bytes materialized per query (256 MB).
/// Allows up to 32 unique 8 MB posting lists before rejecting a query.
const MAX_QUERY_POSTING_BYTES: usize = 256 * 1024 * 1024;

/// Tracks total posting bytes materialized during a single query.
/// Cache hits are free (no new allocation).
pub(crate) struct PostingBudget {
    remaining: Cell<usize>,
}

impl PostingBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            remaining: Cell::new(limit),
        }
    }

    /// Charge `n` bytes against the budget. Returns `Err` if exceeded.
    pub(crate) fn charge(&self, n: usize) -> Result<(), IndexError> {
        let rem = self.remaining.get();
        if n > rem {
            // Not corruption — an OOM defense against a broad/adversarial query
            // on a (possibly large but healthy) index. Don't tell the user to
            // rebuild; tell them to narrow the query.
            return Err(IndexError::QueryTooBroad {
                limit_bytes: MAX_QUERY_POSTING_BYTES,
            });
        }
        self.remaining.set(rem - n);
        Ok(())
    }
}

/// Execute a gram query against base segments plus overlay and return sorted
/// global doc IDs.
pub(crate) fn execute_query(
    query: &GramQuery,
    snap: &IndexSnapshot,
) -> Result<Vec<u32>, IndexError> {
    let budget = PostingBudget::new(MAX_QUERY_POSTING_BYTES);
    Ok(execute_query_bitmap(query, snap, &budget)?.iter().collect())
}

fn execute_query_bitmap(
    query: &GramQuery,
    snap: &IndexSnapshot,
    budget: &PostingBudget,
) -> Result<RoaringBitmap, IndexError> {
    match query {
        GramQuery::And(children) => {
            let mut ordered: Vec<_> = children.iter().collect();
            // Cache the key: query_cardinality_upper_bound is recursive and sums
            // over all base segments at each Grams leaf, so recomputing it per
            // comparison (sort_unstable_by_key) is O(n log n) evaluations of an
            // expensive function. sort_by_cached_key evaluates it once per child.
            ordered.sort_by_cached_key(|child| query_cardinality_upper_bound(child, snap));
            let mut iter = ordered.into_iter();
            let Some(first) = iter.next() else {
                return Ok(snap.all_doc_ids().clone());
            };
            let mut acc = execute_query_bitmap(first, snap, budget)?;
            for child in iter {
                let child_bitmap = execute_query_bitmap(child, snap, budget)?;
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
                let child_bitmap = execute_query_bitmap(child, snap, budget)?;
                acc |= &child_bitmap;
            }
            Ok(acc)
        }
        GramQuery::Grams(hashes) => {
            let mut ordered = hashes.to_vec();
            // gram_cardinality sums over all base segments; cache it per hash.
            ordered.sort_by_cached_key(|&hash| gram_cardinality(hash, snap));
            let mut iter = ordered.into_iter();
            let Some(first) = iter.next() else {
                return Ok(snap.all_doc_ids().clone());
            };
            let first_bm = posting_bitmap_budgeted(first, snap, budget)?;
            let mut acc: RoaringBitmap = if let Some(second) = iter.next() {
                let second_bm = posting_bitmap_budgeted(second, snap, budget)?;
                first_bm.as_ref() & second_bm.as_ref()
            } else {
                first_bm.as_ref().clone()
            };
            for hash in iter {
                if acc.is_empty() {
                    break;
                }
                let postings = posting_bitmap_budgeted(hash, snap, budget)?;
                acc &= postings.as_ref();
            }
            Ok(acc)
        }
        GramQuery::All => Ok(snap.all_doc_ids().clone()),
        GramQuery::None => Ok(RoaringBitmap::new()),
    }
}

pub(crate) fn gram_cardinality(gram_hash: u64, snap: &IndexSnapshot) -> u32 {
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

fn query_cardinality_upper_bound(query: &GramQuery, snap: &IndexSnapshot) -> u32 {
    let total_docs = snap.all_doc_ids().len() as u32;
    match query {
        GramQuery::And(children) => children
            .iter()
            .map(|child| query_cardinality_upper_bound(child, snap))
            .min()
            .unwrap_or(total_docs),
        GramQuery::Or(children) => children
            .iter()
            .fold(0u32, |acc, child| {
                acc.saturating_add(query_cardinality_upper_bound(child, snap))
            })
            .min(total_docs),
        GramQuery::Grams(hashes) => hashes
            .iter()
            .map(|&hash| gram_cardinality(hash, snap))
            .min()
            .unwrap_or(total_docs),
        GramQuery::All => total_docs,
        GramQuery::None => 0,
    }
}

pub(crate) fn is_selective_enough(candidate_count: u64, total_docs: u64, threshold: f64) -> bool {
    (candidate_count as f64) <= (total_docs as f64) * threshold
}

/// Load a posting bitmap with budget tracking. Cache hits are free.
fn posting_bitmap_budgeted(
    gram_hash: u64,
    snap: &IndexSnapshot,
    budget: &PostingBudget,
) -> Result<Arc<RoaringBitmap>, IndexError> {
    if let Some(bitmap) = snap.cached_posting_bitmap(gram_hash) {
        return Ok(bitmap);
    }
    let bitmap = posting_bitmap_inner(gram_hash, snap)?;
    budget.charge(bitmap.serialized_size())?;
    Ok(bitmap)
}

/// Load a posting bitmap without budget tracking. Used by selectivity probing
/// (`should_use_index`) which reads at most 3 posting lists by design.
pub(crate) fn posting_bitmap(
    gram_hash: u64,
    snap: &IndexSnapshot,
) -> Result<Arc<RoaringBitmap>, IndexError> {
    if let Some(bitmap) = snap.cached_posting_bitmap(gram_hash) {
        return Ok(bitmap);
    }
    posting_bitmap_inner(gram_hash, snap)
}

fn posting_bitmap_inner(
    gram_hash: u64,
    snap: &IndexSnapshot,
) -> Result<Arc<RoaringBitmap>, IndexError> {
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
    Ok(snap.store_posting_bitmap(gram_hash, Arc::new(bitmap)))
}

/// Return true if the literal looks selective enough to justify indexed
/// execution instead of a full scan.
pub(crate) fn should_use_index(hashes: &[u64], snap: &IndexSnapshot) -> Result<bool, IndexError> {
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

