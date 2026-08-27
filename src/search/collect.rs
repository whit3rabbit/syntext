//! Post-search collection: grouping, sorting, and doc-id helpers shared by
//! the search executor.

use std::sync::Arc;

use super::{IndexSnapshot, MatchedFile, SearchMatch, SearchOutcome};

/// Group a content-capturing [`SearchOutcome`] into per-file results, moving
/// each file's verified content into its group.
///
/// `matches` is already sorted by `(path, line)`, so a single linear pass
/// groups it; matches within a group stay line-sorted. Requires the outcome to
/// come from `capture_content=true`: every path in `matches` then has a `files`
/// entry. The empty-content fallback is unreachable from the public
/// `SearchOptions` (symbol/refs lookups, which carry no content map, are
/// CLI-only), hence the `debug_assert`.
pub(crate) fn group_outcome(outcome: SearchOutcome) -> Vec<crate::FileMatches> {
    let SearchOutcome { matches, mut files } = outcome;
    let mut groups: Vec<crate::FileMatches> = Vec::new();
    for m in matches {
        if let Some(g) = groups.last_mut() {
            if g.path == m.path {
                g.matches.push(m);
                continue;
            }
        }
        let path = m.path.clone();
        let content: Arc<[u8]> = files
            .remove(&path)
            .map(|mf| mf.normalized)
            .unwrap_or_else(|| {
                debug_assert!(
                    false,
                    "capture_content=true guarantees content for every matched path"
                );
                Arc::from(&[][..])
            });
        groups.push(crate::FileMatches {
            path,
            matches: vec![m],
            content,
        });
    }
    groups
}

/// Per-candidate result: the file's verified content (when captured) plus its
/// matches. Merged after the parallel pass into the final `SearchOutcome`.
pub(super) struct FileResult {
    pub(super) file: Option<(std::path::PathBuf, MatchedFile)>,
    pub(super) matches: Vec<SearchMatch>,
}

/// Sort matches by path (lexicographic), then by line number ascending.
pub(super) fn sort_matches(mut matches: Vec<SearchMatch>) -> Vec<SearchMatch> {
    matches.sort_unstable_by(|a, b| {
        crate::path_util::cmp_path_bytes(&a.path, &b.path)
            .then_with(|| a.line_number.cmp(&b.line_number))
    });
    matches
}

/// All global doc IDs across base segments + overlay, excluding delete_set.
pub(super) fn all_doc_ids(snap: &IndexSnapshot) -> Vec<u32> {
    snap.all_doc_ids().iter().collect()
}
