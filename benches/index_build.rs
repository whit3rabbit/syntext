#[path = "support/mod.rs"]
mod support;

use std::fs;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};

use support::{create_synthetic_repo, mutable_bench_setup};

fn index_build_bench(c: &mut Criterion) {
    let repo = create_synthetic_repo(300);

    let mut group = c.benchmark_group("index_build");
    group.sample_size(10);

    group.bench_function("full_build_300_files", |b| {
        b.iter_batched(
            || tempfile::tempdir().unwrap(),
            |index_dir| {
                let config = syntext::Config {
                    index_dir: index_dir.path().to_path_buf(),
                    repo_root: repo.path().to_path_buf(),
                    ..syntext::Config::default()
                };
                let index = syntext::index::Index::build(config).unwrap();
                black_box(index.stats());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("commit_batch_single_edit", |b| {
        let (_repo, _index_dir, index, target) = mutable_bench_setup(300);
        let mut toggle = false;
        b.iter(|| {
            let content = if toggle {
                "pub fn commit_toggle_alpha() -> usize { 1 }\n"
            } else {
                "pub fn commit_toggle_beta() -> usize { 2 }\n"
            };
            toggle = !toggle;
            fs::write(&target, content).unwrap();
            index.notify_change(&target).unwrap();
            index.commit_batch().unwrap();
            black_box(index.stats());
        });
    });

    group.finish();
}

criterion_group!(benches, index_build_bench);
criterion_main!(benches);
