//! Bench-freshness: incremental git-change detection cost in isolation from
//! the full 100k-file `open_search_e2e` end-to-end target.
//!
//! `open_search_e2e` bundles index-open + detect + search on a 100k-file
//! corpus and is expensive enough that it only runs nightly (see
//! `.github/workflows/nightly.yml`). This target isolates just the
//! freshness-detection cost (`detect_changed_files`) and the bounded
//! `Index::update_from_git` apply path on a much smaller corpus, so it is
//! cheap enough to run locally or on every PR while still tracking the two
//! costs that matter for interactive `st search` latency:
//!
//! 1. `detect_changed_files`: the three bounded git subprocess calls, with
//!    and without a pending change to apply.
//! 2. `Index::update_from_git`: detection plus applying the change to the
//!    overlay, bounded by the same `UpdateLimits` the CLI uses by default
//!    (`max_files: 200`, `budget_ms: 150`; see `cli/config.rs`).

#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use support::{build_index_for_repo, create_synthetic_git_repo, create_synthetic_repo};
use syntext::index::freshness::{detect_changed_files, UpdateLimits};

/// Resolve `git` the simple way: `Command::new` already performs a PATH
/// search for a bare program name on every supported platform, so a bench
/// (unlike the shipped CLI) does not need the crate-internal
/// `git_util::resolve_git_binary` Windows/PATHEXT fallback logic.
fn git_binary() -> PathBuf {
    PathBuf::from("git")
}

fn freshness_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_freshness");
    group.sample_size(10);

    let git = git_binary();

    // Steady-state case: index is fresh, nothing changed since the last
    // commit. This is the common case a search pays on every invocation.
    let clean_repo = create_synthetic_git_repo(2_000);
    let clean_root = clean_repo.path().canonicalize().unwrap();
    group.bench_function("detect_no_changes_2000_files", |b| {
        b.iter(|| {
            black_box(detect_changed_files(&clean_root, &git, None).unwrap());
        });
    });

    // Dirty case: one file has been modified since the last commit, toggled
    // between two contents each iteration so every sample does real detection
    // work rather than hitting an unrealistic no-op after the first run.
    let dirty_repo = create_synthetic_git_repo(2_000);
    let dirty_root = dirty_repo.path().canonicalize().unwrap();
    let dirty_target = dirty_repo.path().join("src/rust/module_0000.rs");
    let mut toggle = false;
    group.bench_function("detect_one_changed_file_2000_files", |b| {
        b.iter(|| {
            let content = if toggle {
                "pub fn freshness_toggle_alpha() -> usize { 1 }\n"
            } else {
                "pub fn freshness_toggle_beta() -> usize { 2 }\n"
            };
            toggle = !toggle;
            fs::write(&dirty_target, content).unwrap();
            black_box(detect_changed_files(&dirty_root, &git, None).unwrap());
        });
    });

    // Bounded apply path: `Index::update_from_git` with the same limits the
    // CLI uses by default (`cli/config.rs`: max_files=200, budget_ms=150).
    // Measures detect + overlay-apply together, on a corpus small enough to
    // run every PR (unlike the 100k-file `open_search_e2e` nightly target).
    let update_repo = create_synthetic_git_repo(2_000);
    let (update_index_dir, index) = build_index_for_repo(update_repo.path());
    drop(index);
    let config = syntext::Config {
        index_dir: update_index_dir.path().to_path_buf(),
        repo_root: update_repo.path().to_path_buf(),
        ..syntext::Config::default()
    };
    let update_target = update_repo.path().join("src/rust/module_0001.rs");
    let limits = UpdateLimits {
        max_files: Some(200),
        budget_ms: Some(150),
    };
    let mut update_toggle = false;
    group.bench_function("update_from_git_bounded_2000_files", |b| {
        let index = syntext::index::Index::open(config.clone()).unwrap();
        b.iter(|| {
            let content = if update_toggle {
                "pub fn update_toggle_alpha() -> usize { 1 }\n"
            } else {
                "pub fn update_toggle_beta() -> usize { 2 }\n"
            };
            update_toggle = !update_toggle;
            fs::write(&update_target, content).unwrap();
            black_box(index.update_from_git(limits.clone()).unwrap());
        });
        drop(index);
    });

    group.finish();
}

/// Delta-commit cost against a LARGE overlay. This isolates the per-commit
/// overlay gram-index cost that the single-file `update_from_git` bench above
/// does not exercise (there the overlay never exceeds ~1 doc). It seeds a
/// several-hundred-doc overlay, then times a single-file delta commit. Before
/// the Arc-shared posting lists change, each such commit deep-cloned every
/// carried-forward posting list (O(total overlay grams)); after, the map clone
/// is refcount bumps and only the changed file's lists are copied
/// (O(changed-file grams)).
fn overlay_delta_commit_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_freshness");
    group.sample_size(10);

    // 2000-file base -> overlay cap is ~1000 docs; seed 800 new files so the
    // carried-forward overlay is large but under the enforcement threshold.
    let repo = create_synthetic_repo(2_000);
    let (_index_dir, index) = build_index_for_repo(repo.path());
    for i in 0..800 {
        let path = repo.path().join(format!("src/rust/overlay_seed_{i:04}.rs"));
        fs::write(
            &path,
            format!("pub fn overlay_seed_{i}(x: usize) -> usize {{ x + {i} }}\n"),
        )
        .unwrap();
        index.notify_change(&path).unwrap();
    }
    index.commit_batch().unwrap();

    // Time a single-file delta commit against that large overlay.
    let target = repo.path().join("src/rust/overlay_seed_0000.rs");
    let mut toggle = false;
    group.bench_function("overlay_delta_commit_800_doc_overlay", |b| {
        b.iter(|| {
            let content = if toggle {
                "pub fn overlay_seed_0000_alpha() -> usize { 1 }\n"
            } else {
                "pub fn overlay_seed_0000_beta() -> usize { 2 }\n"
            };
            toggle = !toggle;
            fs::write(&target, content).unwrap();
            index.notify_change(&target).unwrap();
            // commit_batch mutates the index and does I/O, so it is not elided;
            // no black_box needed (and it returns unit, which black_box warns on).
            index.commit_batch().unwrap();
        });
    });

    group.finish();
    drop(index);
}

criterion_group!(benches, freshness_bench, overlay_delta_commit_bench);
criterion_main!(benches);
