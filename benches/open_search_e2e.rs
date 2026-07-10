//! End-to-end bench: index open + bounded git change detection + one search,
//! on a synthetic 100k-file git repository.
//!
//! This tracks the cost path a real `st search` invocation pays when
//! `auto_update` is enabled (the default): open the on-disk index, run the
//! bounded `update_from_git` detection (three git commands under a time
//! budget), then execute a single query against the resulting snapshot.
//! Regressions here show up as slower interactive searches, so this target
//! is wired into a nightly CI gate (see `.github/workflows/nightly.yml`)
//! rather than the PR-blocking bench suite, since a 100k-file corpus is too
//! slow to build on every PR.

#[path = "support/mod.rs"]
mod support;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use support::{build_index_for_repo, create_synthetic_git_repo};
use syntext::index::freshness::UpdateLimits;

fn open_search_e2e_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("open_search_e2e");
    group.sample_size(10);

    // Setup (untimed): generate the 100k-file corpus once, as a git repo so
    // `update_from_git` has a HEAD to diff against, and build the on-disk
    // index once. Each bench iteration only pays open + detect + search.
    let repo = create_synthetic_git_repo(100_000);
    let (index_dir, index) = build_index_for_repo(repo.path());
    // Windows: release locks/mmaps before re-opening the same directory.
    drop(index);

    let config = syntext::Config {
        index_dir: index_dir.path().to_path_buf(),
        repo_root: repo.path().to_path_buf(),
        ..syntext::Config::default()
    };
    let opts = syntext::SearchOptions::default();
    let limits = UpdateLimits {
        max_files: Some(200),
        budget_ms: Some(150),
    };

    group.bench_function("open_detect_search_100k_files", |b| {
        b.iter(|| {
            let index = syntext::index::Index::open(config.clone()).unwrap();
            // Bounded git-detection step: mirrors `run_bounded_auto_update` in
            // `cli/catchup.rs`, which every `st search` call performs by
            // default before executing the query.
            black_box(index.update_from_git(limits.clone()).unwrap());
            black_box(index.search("parse_query", &opts).unwrap().len());
            drop(index);
        });
    });

    group.finish();
}

criterion_group!(benches, open_search_e2e_bench);
criterion_main!(benches);
