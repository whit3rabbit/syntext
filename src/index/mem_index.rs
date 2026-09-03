//! Mutable in-memory document index (chat-content search, `ffi` feature).
//!
//! [`MemIndex`] buffers caller-supplied documents (`id -> content`) and
//! rebuilds a pure-overlay snapshot on [`MemIndex::commit`]. Searches load the
//! current snapshot, so a commit publishes atomically: in-flight searches
//! finish against the pre-commit snapshot.
//!
//! Cost model: `commit` is O(total indexed content) (a full snapshot rebuild,
//! not an incremental overlay update). That is the right trade for chat-scale
//! corpora (thousands of small documents); for large corpora use the native
//! directory [`Index`](crate::index::Index) instead.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use arc_swap::ArcSwap;

use crate::index::snapshot::IndexSnapshot;
use crate::index::wasm_index::{build_overlay_snapshot, validate_doc_id};
use crate::{Config, IndexError, SearchMatch, SearchOptions};

/// Mutable in-memory document index for FFI consumers.
///
/// All methods take `&self`; the type is `Send + Sync` (asserted in tests).
/// `add`/`remove` buffer edits; nothing is visible to `search` until
/// [`commit`](MemIndex::commit) succeeds.
pub struct MemIndex {
    docs: RwLock<HashMap<String, Arc<[u8]>>>,
    snapshot: ArcSwap<IndexSnapshot>,
    config: Config,
}

// Poison recovery: a panic inside commit's build path cannot corrupt the docs
// map (it only reads), so into_inner recovery matches the PendingEdits
// convention (see src/index/pending.rs).
impl MemIndex {
    /// Create an empty index.
    ///
    /// Infallible today; the `Result` reserves room for future
    /// construction-time validation without a breaking change.
    pub fn new() -> Result<Self, IndexError> {
        let snapshot = build_overlay_snapshot(&HashMap::new())?;
        Ok(MemIndex {
            docs: RwLock::new(HashMap::new()),
            snapshot: ArcSwap::from(snapshot),
            config: Config::default(),
        })
    }

    /// Buffer a document, replacing any existing entry with the same id.
    ///
    /// The id doubles as the index path, so traversal-shaped ids (`..`,
    /// leading `/`, Windows prefixes) and empty ids are rejected with
    /// [`IndexError::PathOutsideRepo`] (same guard as the wasm index: ids
    /// become index paths, so the traversal check is security-relevant).
    /// Binary-looking content is not rejected here; it is skipped at commit
    /// time by the same heuristic the wasm index uses.
    ///
    /// Not visible to searches until [`commit`](MemIndex::commit).
    pub fn add(&self, id: &str, content: Arc<[u8]>) -> Result<(), IndexError> {
        validate_doc_id(id)?;
        self.docs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), content);
        Ok(())
    }

    /// Buffer a deletion. Removing an absent id is a no-op.
    ///
    /// Not visible to searches until [`commit`](MemIndex::commit).
    pub fn remove(&self, id: &str) -> Result<(), IndexError> {
        validate_doc_id(id)?;
        self.docs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        Ok(())
    }

    /// Rebuild the snapshot from all buffered documents and publish it.
    ///
    /// Holds the docs read lock for the whole build, blocking `add`/`remove`
    /// for the duration. O(total content). On error the previous snapshot
    /// stays live (build-then-swap).
    pub fn commit(&self) -> Result<(), IndexError> {
        let docs = self.docs.read().unwrap_or_else(|e| e.into_inner());
        let next = build_overlay_snapshot(&docs)?;
        self.snapshot.store(next);
        Ok(())
    }

    /// Search the committed snapshot for `pattern` (literal or regex).
    pub fn search(
        &self,
        pattern: &str,
        opts: &SearchOptions,
    ) -> Result<Vec<SearchMatch>, IndexError> {
        // canonical_root is unused for pure-overlay snapshots (the resolver
        // returns overlay content directly without touching the filesystem).
        let canonical_root = std::path::Path::new(".");
        crate::search::search(
            self.snapshot.load_full(),
            &self.config,
            canonical_root,
            pattern,
            opts,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(bytes: &[u8]) -> Arc<[u8]> {
        Arc::from(bytes.to_vec().into_boxed_slice())
    }

    fn search_count(idx: &MemIndex, pattern: &str) -> usize {
        idx.search(pattern, &SearchOptions::default())
            .expect("search must succeed")
            .len()
    }

    #[test]
    fn mem_index_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemIndex>();
    }

    #[test]
    fn empty_index_searches_to_nothing() {
        let idx = MemIndex::new().expect("new");
        assert_eq!(search_count(&idx, "anything"), 0);
    }

    #[test]
    fn add_is_visible_only_after_commit() {
        let idx = MemIndex::new().expect("new");
        idx.add("chats/a", content(b"the needle hides here"))
            .expect("add");
        assert_eq!(search_count(&idx, "needle"), 0, "uncommitted add visible");
        idx.commit().expect("commit");
        assert_eq!(search_count(&idx, "needle"), 1, "committed add missing");
    }

    #[test]
    fn remove_stops_matching_after_commit() {
        let idx = MemIndex::new().expect("new");
        idx.add("chats/a", content(b"the needle hides here"))
            .expect("add");
        idx.commit().expect("commit");
        idx.remove("chats/a").expect("remove");
        assert_eq!(
            search_count(&idx, "needle"),
            1,
            "uncommitted remove visible"
        );
        idx.commit().expect("commit");
        assert_eq!(
            search_count(&idx, "needle"),
            0,
            "removed doc still matching"
        );
    }

    #[test]
    fn same_id_replace_reindexes() {
        let idx = MemIndex::new().expect("new");
        idx.add("chats/a", content(b"first version mentions zebra"))
            .expect("add");
        idx.commit().expect("commit");
        idx.add("chats/a", content(b"second version mentions yak"))
            .expect("replace");
        idx.commit().expect("commit");
        assert_eq!(search_count(&idx, "zebra"), 0, "old content still indexed");
        assert_eq!(search_count(&idx, "yak"), 1, "new content missing");
    }

    #[test]
    fn rejects_traversal_and_empty_ids() {
        let idx = MemIndex::new().expect("new");
        for bad in ["../x", "/x", "a/../b", ""] {
            match idx.add(bad, content(b"m")) {
                Err(IndexError::PathOutsideRepo(_)) => {}
                other => panic!("expected PathOutsideRepo for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_degenerate_separator_ids() {
        let idx = MemIndex::new().expect("new");
        for bad in ["chats/1/", "chats//1", "chats/./1", "."] {
            match idx.add(bad, content(b"m")) {
                Err(IndexError::PathOutsideRepo(_)) => {}
                other => panic!("expected PathOutsideRepo for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn binary_content_skipped_at_commit() {
        let idx = MemIndex::new().expect("new");
        // NUL-heavy content trips the binary heuristic (same as wasm index).
        idx.add(
            "chats/bin",
            content(b"needle\x00\x01\x02\x00\x03\x00\x04\x00"),
        )
        .expect("add");
        idx.commit().expect("commit");
        assert_eq!(search_count(&idx, "needle"), 0);
    }
}
