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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use resolver::resolve_doc;

#[cfg(feature = "rayon")]
use rayon::prelude::*;
use regex::bytes::RegexBuilder;

use crate::index::IndexSnapshot;
use crate::path::filter::{build_filter, matches_path_filter};
use crate::query::{literal_grams, route_query, GramQuery, QueryRoute};
use crate::{Config, IndexError, SearchMatch, SearchOptions};

use executor::{execute_query, should_use_index};

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

use verifier::{verify_empty, verify_literal, verify_regex};

/// Verified content for a file that produced at least one match, captured
/// exactly as the verifier saw it. Lets renderers emit the bytes that matched
/// instead of re-reading (and possibly re-normalizing differently, or missing)
/// a file that churned between search and render.
#[derive(Clone)]
pub(crate) struct MatchedFile {
    /// Encoding-normalized bytes the verifier matched against.
    pub normalized: Arc<[u8]>,
    /// On-disk raw byte length (pre-normalize) for `bytes_searched`.
    pub raw_len: u64,
}

/// Search matches plus the verified content of every file that produced one.
/// `files` is populated only when content capture is requested; it always
/// contains an entry for each path present in `matches`.
pub(crate) struct SearchOutcome {
    pub matches: Vec<SearchMatch>,
    pub files: HashMap<PathBuf, MatchedFile>,
}

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
    Ok(search_with_content(snap, config, canonical_root, pattern, opts, false)?.matches)
}

/// Like [`search`], but also returns the verified content of matched files when
/// `capture_content` is true. Renderers that re-read file bytes use this to stay
/// consistent with the match snapshot. When `capture_content` is false, the
/// returned `files` map is empty and no content is retained.
pub(crate) fn search_with_content(
    snap: Arc<IndexSnapshot>,
    config: &Config,
    canonical_root: &std::path::Path,
    pattern: &str,
    opts: &SearchOptions,
    capture_content: bool,
) -> Result<SearchOutcome, IndexError> {
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
            // CRLF mode: `^`/`$` treat a bare `\r` (and `\r\n`) as a line
            // terminator, matching the oracle's `rg --crlf`. Makes `-x parse`
            // match a final line "parse\r" (`$` matches zero-width before the
            // `\r`, so line_content stays "parse" and the span guard holds). A
            // dangling odd byte in truncated UTF-16 decodes to U+FFFD, not `\r`
            // (see index/encoding.rs), so it stays content and does not match.
            .crlf(true)
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
                GramQuery::Grams(grams) if !grams.is_empty() => should_use_index(grams, &snap)?,
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

    let glob_cache = snap
        .glob_cache
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    // Combine the single-type convenience fields with the multi-type vecs so a
    // library caller can use either; the index narrows on all of them.
    let include_types: Vec<&str> = opts
        .file_type
        .as_deref()
        .into_iter()
        .chain(opts.file_types.iter().map(String::as_str))
        .collect();
    let exclude_types: Vec<&str> = opts
        .exclude_type
        .as_deref()
        .into_iter()
        .chain(opts.exclude_types.iter().map(String::as_str))
        .collect();
    let path_filter_bitmap = build_filter(
        &snap.path_index,
        &include_types,
        &exclude_types,
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
    let deterministic = opts.deterministic;
    let match_count = AtomicUsize::new(0);
    // Repo-root anchor for the Linux openat2 fast path, opened once per search
    // and shared read-only across the parallel candidates (None off Linux, on
    // an openat2-less kernel, or under SYNTEXT_NO_OPENAT2).
    let root_fd = crate::index::io_util::open_root_dirfd(canonical_root);
    let do_match = |&global_id: &u32| -> Option<FileResult> {
        // Early-exit: skip expensive I/O once we already have enough matches.
        if let Some(limit) = opts.max_results {
            if !deterministic && match_count.load(Ordering::Relaxed) >= limit {
                return None;
            }
        }
        let (rel_path, content, raw_len) = resolve_doc(
            &snap,
            global_id,
            canonical_root,
            root_fd.as_ref(),
            config.max_file_size,
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
                    &include_types,
                    &exclude_types,
                    opts.path_filter.as_deref(),
                ) {
                    return None;
                }
            }
        }

        let file_path = rel_path.as_path();
        let file_matches = if verify_pattern.is_empty() {
            verify_empty(file_path, &content, opts.skip_line_content)
        } else {
            match &route {
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
            }
        };
        if let Some(_limit) = opts.max_results {
            if !file_matches.is_empty() {
                match_count.fetch_add(file_matches.len(), Ordering::Relaxed);
            }
        }
        // Capture verified content only for files that actually matched, and
        // only when a content renderer will consume it (an Arc::clone, no copy).
        let file = if capture_content && !file_matches.is_empty() {
            Some((
                rel_path,
                MatchedFile {
                    normalized: Arc::clone(&content),
                    raw_len,
                },
            ))
        } else {
            None
        };
        Some(FileResult {
            file,
            matches: file_matches,
        })
    };
    #[cfg(feature = "rayon")]
    let per_file: Vec<FileResult> = if let Some(ref pool) = config.thread_pool {
        pool.install(|| candidates.par_iter().filter_map(do_match).collect())
    } else {
        candidates.par_iter().filter_map(do_match).collect()
    };
    #[cfg(not(feature = "rayon"))]
    let per_file: Vec<FileResult> = candidates.iter().filter_map(do_match).collect();

    // Content capture is the exception, not the rule: every plain `Index::search`
    // passes `capture_content=false`. Only pay for the path->content map (and its
    // per-file merge) when a renderer will consume it; otherwise flatten the
    // matches straight through as the pre-capture code did.
    let mut files: HashMap<PathBuf, MatchedFile> = HashMap::new();
    let all_matches: Vec<SearchMatch> = if capture_content {
        let mut acc = Vec::new();
        for fr in per_file {
            if let Some((path, mf)) = fr.file {
                files.insert(path, mf);
            }
            acc.extend(fr.matches);
        }
        acc
    } else {
        per_file.into_iter().flat_map(|fr| fr.matches).collect()
    };

    let mut matches = sort_matches(all_matches);
    if let Some(max) = opts.max_results {
        matches.truncate(max);
        // Truncation can drop whole files; prune their captured content so we
        // don't pin bytes for files that won't be rendered (e.g. `-m 1` over a
        // 10k-file result set).
        if capture_content {
            let live: std::collections::HashSet<&Path> =
                matches.iter().map(|m| m.path.as_path()).collect();
            files.retain(|p, _| live.contains(p.as_path()));
        }
    }
    Ok(SearchOutcome { matches, files })
}

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
struct FileResult {
    file: Option<(PathBuf, MatchedFile)>,
    matches: Vec<SearchMatch>,
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

#[cfg(test)]
mod tests;
