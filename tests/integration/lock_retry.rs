//! Bounded retry for transient `write.lock` contention in integration tests.
//!
//! Shared across test targets via `#[path = "lock_retry.rs"] mod lock_retry;`
//! (same mechanism as `oracle_helpers.rs`). Each target compiles its own copy,
//! so the calibrated budget is per test binary.
//!
//! Why tests need this at all: `Index::build` drops `write.lock` before it
//! returns, so in a single-threaded world the very next writer cannot block.
//! It did on CI anyway (macos-14 on three consecutive runs, ubuntu once, all
//! `WouldBlock`, all on a private temp dir no other test opens). The tests in
//! a binary run in parallel threads and several of them spawn `git`
//! (`Index::build` itself spawns `git rev-parse` for `base_commit`). A child
//! forked while another thread holds `write.lock` open inherits that
//! descriptor until it execs, and a `flock` lives as long as any descriptor
//! to its open file, so the parent's drop does not release it until the
//! child's exec closes the copy. That window is microseconds on an idle box
//! and long enough to lose a race on a loaded runner.
#![allow(dead_code)]

use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use syntext::index::Index;
use syntext::IndexError;

/// Retry budget for transient `write.lock` contention, calibrated from a
/// measured warm-up spawn instead of a fixed constant. CLAUDE.md
/// ("Load-sensitive tests") is explicit that a fixed deadline only moves the
/// flake threshold: the exec latency this waits out is microseconds idle and
/// 3.8s-10s+ on a loaded macOS runner under syspolicyd. Measured once per
/// test binary, x10 and clamped, same shape as `calibrated_kill_deadline_ms`
/// (`src/index/freshness_tests.rs`).
pub fn lock_retry_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let start = Instant::now();
        let _ = std::process::Command::new("git").arg("--version").output();
        let observed_ms = start.elapsed().as_millis().min(3_000) as u64;
        Duration::from_millis((observed_ms * 10).clamp(500, 30_000))
    })
}

pub fn is_lock_conflict(err: &IndexError) -> bool {
    matches!(err, IndexError::LockConflict(_))
}

/// Run `op` until it succeeds, until `retry_on` says the error is not
/// transient lock contention, or until the calibrated `lock_retry_budget`
/// is spent. Backoff is exponential (10ms doubling, 250ms per sleep), so an
/// idle-box success costs exactly one call and a loaded box spreads its
/// waits across the whole budget. Returns the last error otherwise.
pub fn retry_lock_contention<T, E>(
    mut op: impl FnMut() -> Result<T, E>,
    retry_on: impl Fn(&E) -> bool,
) -> Result<T, E> {
    let deadline = Instant::now() + lock_retry_budget();
    let mut delay = Duration::from_millis(10);
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) if !retry_on(&err) => return Err(err),
            Err(err) if Instant::now() >= deadline => return Err(err),
            Err(_) => {
                thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(250));
            }
        }
    }
}

/// `commit_batch`, retrying `LockConflict` within the calibrated budget.
/// Panics on any other error or once the budget is spent.
pub fn commit_batch_with_retry(index: &Index) {
    retry_lock_contention(|| index.commit_batch(), is_lock_conflict)
        .unwrap_or_else(|err| panic!("commit_batch failed within retry budget: {err}"));
}

/// Like `commit_batch_with_retry`, but returns the result instead of
/// panicking on non-LockConflict errors. Used by tests that assert on
/// specific error variants.
pub fn commit_batch_result(index: &Index) -> Result<(), IndexError> {
    retry_lock_contention(|| index.commit_batch(), is_lock_conflict)
}

/// Take an exclusive `flock` on a test-owned `write.lock`, retrying on
/// `WouldBlock` within the calibrated budget. Retrying the first lock after a
/// build is the same remedy `commit_batch_with_retry` applies to the same
/// symptom (see the module docs).
pub fn retry_transient_write_lock(lock_file: &std::fs::File) {
    retry_lock_contention(
        || lock_file.try_lock(),
        |err| matches!(err, std::fs::TryLockError::WouldBlock),
    )
    .unwrap_or_else(|err| panic!("try_lock on write.lock failed within retry budget: {err}"));
}
