//! Bounded `LockConflict` retry for the read-only search open path.
//!
//! `Index::open` takes a shared lock on the index dir, so it fails while any
//! writer holds the exclusive one. `st` creates that contention itself: a
//! search whose bounded auto-update overruns its budget spawns a detached
//! `st update --quiet` catch-up (see `catchup::maybe_spawn_async_catchup`),
//! and the *next* search can land inside that child's exclusive window and
//! exit 2 with "index locked by another process" -- a failure the user did
//! nothing to cause and cannot predict.
//!
//! The exclusive windows `st` opens on itself (a delta flush, installing a
//! rebuilt index) are short, so a small bounded wait absorbs them. This is
//! deliberately not an unbounded block: a genuine long writer (`st index` on
//! a large repo, a compaction) still surfaces the same loud error, just after
//! ~500ms instead of immediately.
//!
//! Only `LockConflict` is retried. `CorruptIndex` and every other error still
//! return on the first attempt, so this never masks real corruption -- the
//! property `search.rs` fails loudly to protect.

use std::time::Duration;

use crate::index::Index;
use crate::{Config, IndexError};

/// Backoff schedule between `LockConflict` retries, in milliseconds.
///
/// Explicit rather than computed so the total wait is readable at a glance:
/// five retries, 500ms worst case. That is under the threshold where a
/// developer reads a search as hung, and comfortably covers the sub-100ms
/// exclusive windows of a background `st update` on a small-to-medium repo.
const LOCK_RETRY_BACKOFF_MS: &[u64] = &[20, 40, 80, 160, 200];

/// Open the index for a read-only search, retrying a transient
/// [`IndexError::LockConflict`] on the schedule above.
pub(super) fn open_for_search(config: &Config) -> Result<Index, IndexError> {
    retry_lock_conflict(
        || Index::open(config.clone()),
        std::thread::sleep,
        LOCK_RETRY_BACKOFF_MS,
    )
}

/// Retry `attempt` while it reports `LockConflict`, sleeping the given backoff
/// between tries. Generic over the sleep so the schedule is unit-testable
/// without real time (the same shape `index::helpers::classify_try_lock`
/// uses to test its own retry policy).
fn retry_lock_conflict<T>(
    mut attempt: impl FnMut() -> Result<T, IndexError>,
    mut sleep: impl FnMut(Duration),
    backoff_ms: &[u64],
) -> Result<T, IndexError> {
    let mut last = attempt();
    for delay in backoff_ms {
        match last {
            Err(IndexError::LockConflict(_)) => {}
            other => return other,
        }
        sleep(Duration::from_millis(*delay));
        last = attempt();
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn conflict() -> IndexError {
        IndexError::LockConflict(PathBuf::from("/tmp/idx"))
    }

    /// Records the backoff actually slept so the schedule is asserted, not
    /// just the call count.
    fn run(
        outcomes: Vec<Result<u32, IndexError>>,
    ) -> (Result<u32, IndexError>, usize, Vec<u64>) {
        let remaining = RefCell::new(outcomes.into_iter());
        let slept = RefCell::new(Vec::new());
        let calls = RefCell::new(0usize);
        let result = retry_lock_conflict(
            || {
                *calls.borrow_mut() += 1;
                remaining.borrow_mut().next().unwrap_or_else(|| Err(conflict()))
            },
            |d| slept.borrow_mut().push(d.as_millis() as u64),
            LOCK_RETRY_BACKOFF_MS,
        );
        let calls = *calls.borrow();
        let slept = slept.borrow().clone();
        (result, calls, slept)
    }

    #[test]
    fn success_on_first_try_never_sleeps() {
        let (result, calls, slept) = run(vec![Ok(7)]);
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls, 1);
        assert!(slept.is_empty(), "an uncontended open must not wait");
    }

    #[test]
    fn transient_conflict_is_retried_until_it_clears() {
        let (result, calls, slept) = run(vec![Err(conflict()), Err(conflict()), Ok(7)]);
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls, 3);
        assert_eq!(slept, vec![20, 40], "backoff must follow the schedule");
    }

    #[test]
    fn persistent_conflict_still_fails_loudly_and_is_bounded() {
        let (result, calls, slept) = run(vec![]);
        assert!(matches!(result, Err(IndexError::LockConflict(_))));
        assert_eq!(calls, LOCK_RETRY_BACKOFF_MS.len() + 1);
        assert_eq!(slept, LOCK_RETRY_BACKOFF_MS.to_vec());
        assert_eq!(slept.iter().sum::<u64>(), 500, "bounded total wait");
    }

    /// The load-bearing property: retrying a lock conflict must not delay or
    /// soften any other error, or a corrupt index would be masked behind a
    /// wait and a different message.
    #[test]
    fn non_lock_errors_return_immediately() {
        let (result, calls, slept) = run(vec![Err(IndexError::CorruptIndex("bad".into()))]);
        assert!(matches!(result, Err(IndexError::CorruptIndex(_))));
        assert_eq!(calls, 1);
        assert!(slept.is_empty());
    }

    #[test]
    fn missing_index_returns_immediately_so_fallback_stays_fast() {
        let (result, calls, slept) = run(vec![Err(IndexError::IndexNotFound(PathBuf::from(
            "/tmp/idx",
        )))]);
        assert!(matches!(result, Err(IndexError::IndexNotFound(_))));
        assert_eq!(calls, 1);
        assert!(slept.is_empty());
    }

    /// End-to-end over the real `Index::open`, against a real held lock: the
    /// tests above pin the policy, this pins that `open_for_search` is the
    /// thing that applies it.
    ///
    /// `Index::open` takes the dir lock before it reads the manifest, so an
    /// empty directory is enough to reach the conflict -- no index build, no
    /// subprocess. Staying in-process is what makes the elapsed floor sound:
    /// the only cost measured is the backoff itself, and scheduling noise can
    /// only push it up. Wall-clocking a spawned `st` would not work, since
    /// exec latency alone reaches seconds when the OS is assessing a
    /// freshly-linked binary.
    #[test]
    fn open_for_search_waits_out_a_real_held_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("lock"))
            .unwrap();
        // A second open of the same path is a distinct file description, so
        // this conflicts with `Index::open`'s shared lock as a separate
        // process would.
        held.try_lock().expect("take exclusive dir lock");

        let config = Config {
            index_dir: dir.path().to_path_buf(),
            repo_root: dir.path().to_path_buf(),
            ..Config::default()
        };
        let start = std::time::Instant::now();
        let result = open_for_search(&config);
        let elapsed = start.elapsed();
        held.unlock().unwrap();

        assert!(
            matches!(result.as_ref().err(), Some(IndexError::LockConflict(_))),
            "a lock held throughout must still surface loudly"
        );
        assert!(
            elapsed >= Duration::from_millis(400),
            "open_for_search returned without exhausting the backoff ({elapsed:?})"
        );
        drop(result);
    }
}
