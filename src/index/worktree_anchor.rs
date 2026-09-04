//! On-disk record of what the working tree looked like at the last durable
//! flush: `worktree-<uuid>.idx`.
//!
//! # Why this exists
//!
//! `freshness::detect_changed_files` asks git which paths differ from HEAD.
//! For *uncommitted* drift the answer never changes: an edited-but-uncommitted
//! file is reported on every single search, forever. Before durable flush that
//! was harmless (the apply was in-memory anyway), but once `st update` writes
//! those files into a delta segment, re-detecting them means re-reading and
//! re-applying content the index already holds, on every search, until the user
//! commits.
//!
//! This sidecar records, per flushed path, what was on disk when it was
//! flushed. [`WorktreeAnchor::is_unchanged`] compares that against a fresh
//! `stat` and lets `Index::retain_unflushed` drop paths that have not moved
//! since.
//!
//! # Fail open, unlike `deletes_idx`
//!
//! Losing this file costs work, never correctness. Every dropped path is
//! simply re-applied, and `pending::compute_delete_set` makes re-applying an
//! already-flushed path idempotent. So a read error here is logged and treated
//! as an empty anchor, the same way `paths.idx` is treated, and the opposite of
//! the fail-closed `deletes_idx` (whose loss produces duplicate results).
//!
//! # The racy-mtime rule
//!
//! Same problem git solves with "racily clean" entries. If a file is written
//! twice within one filesystem timestamp tick, the second write leaves an mtime
//! and size identical to the first, and a stat comparison cannot tell them
//! apart. So a path is anchored only when its mtime is at least
//! [`RACY_MARGIN`] older than the moment the flushed content was read
//! (`FlushBookkeeping::read_epoch`). A path that fails that test is left out of
//! the anchor and re-applied once on the next pass, which is the safe
//! direction.
//!
//! The on-disk format, and the read/write entry points, live in
//! [`super::worktree_codec`].

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Above this many entries no anchor is written at all. A repo with 50k
/// uncommitted paths is doing something a per-path anchor will not rescue, and
/// the sidecar would cost more to read on every update than the re-applies it
/// saves. Falling back to "no anchor" is always correct, just slower.
pub(crate) const MAX_ENTRIES: usize = 50_000;

/// How far an mtime must precede the content read to be trusted. Mirrors git's
/// racily-clean margin: coarse filesystem timestamps (HFS+ at 1s, some network
/// filesystems worse) can stamp two writes in the same tick.
const RACY_MARGIN: Duration = Duration::from_secs(2);

/// What was observed at a path when it was flushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Observed {
    /// Nothing was at this path: a deletion, or a file that vanished between
    /// `notify_change` and the commit's read.
    Absent,
    /// A file was present with this size and mtime. Also used for paths the
    /// commit *excluded* (binary, oversized): they are on disk, they are not
    /// documents, and re-classifying them on every search is the cost this
    /// avoids.
    Present {
        size: u64,
        mtime_secs: i64,
        mtime_nanos: u32,
    },
}

impl Observed {
    /// Observe `abs` right now, or `None` when the stat failed for a reason
    /// other than "not there" (a permission error is not evidence of absence).
    fn stat(abs: &Path) -> Option<Self> {
        match std::fs::metadata(abs) {
            Ok(meta) => {
                let modified = meta.modified().ok()?;
                let (mtime_secs, mtime_nanos) = split_system_time(modified)?;
                Some(Observed::Present {
                    size: meta.len(),
                    mtime_secs,
                    mtime_nanos,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Observed::Absent),
            Err(_) => None,
        }
    }

    /// Whether this observation is settled enough to trust, given the moment
    /// the flushed content was read. See the racy-mtime rule in the module
    /// docs. `Absent` is always settled: there is no content to race with.
    fn is_settled(&self, read_epoch: SystemTime, now: SystemTime) -> bool {
        let Observed::Present {
            mtime_secs,
            mtime_nanos,
            ..
        } = self
        else {
            return true;
        };
        let Some(mtime) = join_system_time(*mtime_secs, *mtime_nanos) else {
            return false;
        };
        // A future mtime (clock skew, a network filesystem, a touched-ahead
        // file) tells us nothing about whether the file settled before the
        // read, so refuse to anchor it.
        if mtime > now {
            return false;
        }
        mtime + RACY_MARGIN < read_epoch
    }
}

fn split_system_time(t: SystemTime) -> Option<(i64, u32)> {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => Some((i64::try_from(d.as_secs()).ok()?, d.subsec_nanos())),
        // Pre-epoch mtime. Representable, just vanishingly rare, and the
        // negative-second encoding is not worth the branch: refuse to anchor.
        Err(_) => None,
    }
}

fn join_system_time(secs: i64, nanos: u32) -> Option<SystemTime> {
    let secs = u64::try_from(secs).ok()?;
    UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

/// What the working tree looked like at the last durable flush.
#[derive(Debug, Default, Clone)]
pub(crate) struct WorktreeAnchor {
    pub(super) entries: HashMap<PathBuf, Observed>,
}

impl WorktreeAnchor {
    /// Construct from a decoded entry map. Only [`super::worktree_codec`]
    /// needs this; every other caller goes through [`Self::build_next`].
    pub(super) fn from_entries(entries: HashMap<PathBuf, Observed>) -> Self {
        WorktreeAnchor { entries }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `rel` is still exactly as it was when last flushed, so
    /// re-applying it would be pure waste.
    ///
    /// Answers `false` for any path this anchor does not cover, and for any
    /// path whose current state cannot be observed. Both are the safe
    /// direction: the caller re-applies.
    pub(crate) fn is_unchanged(&self, root: &Path, rel: &Path) -> bool {
        let Some(recorded) = self.entries.get(rel) else {
            return false;
        };
        Observed::stat(&root.join(rel)) == Some(*recorded)
    }

    /// Build the anchor to persist alongside a flush.
    ///
    /// `flushed` are the paths this flush wrote as documents, `non_docs` are
    /// the paths the commit removed or excluded, and `prev` is the anchor the
    /// previous flush left. Entries in `prev` for paths this flush touched are
    /// replaced; the rest carry forward, because a path flushed two updates ago
    /// and untouched since is still durably indexed.
    ///
    /// Returns `None` when the result would exceed [`MAX_ENTRIES`].
    pub(crate) fn build_next(
        prev: &WorktreeAnchor,
        flushed: &HashSet<PathBuf>,
        non_docs: &HashSet<PathBuf>,
        root: &Path,
        read_epoch: SystemTime,
    ) -> Option<WorktreeAnchor> {
        let now = SystemTime::now();
        let mut entries = prev.entries.clone();
        // A path can be in both sets across several commits (deleted, then
        // recreated). The document wins: it is what actually went into the
        // segment.
        for rel in non_docs.iter().filter(|p| !flushed.contains(*p)) {
            record(&mut entries, root, rel, read_epoch, now);
        }
        for rel in flushed {
            record(&mut entries, root, rel, read_epoch, now);
        }
        if entries.len() > MAX_ENTRIES {
            return None;
        }
        Some(WorktreeAnchor { entries })
    }
}

/// Observe one path and either record it or drop any stale entry for it.
///
/// Dropping matters: a path whose current state cannot be trusted (racy mtime,
/// unreadable) must not keep an *older* entry that might now match by
/// coincidence.
fn record(
    entries: &mut HashMap<PathBuf, Observed>,
    root: &Path,
    rel: &Path,
    read_epoch: SystemTime,
    now: SystemTime,
) {
    match Observed::stat(&root.join(rel)) {
        Some(observed) if observed.is_settled(read_epoch, now) => {
            entries.insert(rel.to_path_buf(), observed);
        }
        _ => {
            entries.remove(rel);
        }
    }
}

#[cfg(test)]
#[path = "worktree_anchor_tests.rs"]
mod tests;
