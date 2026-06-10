#[path = "support/mod.rs"]
mod support;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use support::{build_index_for_repo, create_synthetic_repo};

fn open_config(index_dir: &std::path::Path, repo_root: &std::path::Path) -> syntext::Config {
    syntext::Config {
        index_dir: index_dir.to_path_buf(),
        repo_root: repo_root.to_path_buf(),
        ..syntext::Config::default()
    }
}

fn index_open_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_open");
    group.sample_size(10);

    let repo_300 = create_synthetic_repo(300);
    let (index_dir_300, index) = build_index_for_repo(repo_300.path());
    // Windows: release locks/mmaps before re-opening the same directory.
    drop(index);

    group.bench_function("open_300_files", |b| {
        let config = open_config(index_dir_300.path(), repo_300.path());
        b.iter(|| {
            let index = syntext::index::Index::open(config.clone()).unwrap();
            black_box(index.stats());
            drop(index);
        });
    });

    let repo_2000 = create_synthetic_repo(2000);
    let (index_dir_2000, index) = build_index_for_repo(repo_2000.path());
    drop(index);

    group.bench_function("open_2000_files", |b| {
        let config = open_config(index_dir_2000.path(), repo_2000.path());
        b.iter(|| {
            let index = syntext::index::Index::open(config.clone()).unwrap();
            black_box(index.stats());
            drop(index);
        });
    });

    // Pre-change behavior (full .post checksum on open), kept measurable.
    group.bench_function("open_2000_files_full_verify", |b| {
        let config = syntext::Config {
            verify_on_open: true,
            ..open_config(index_dir_2000.path(), repo_2000.path())
        };
        b.iter(|| {
            let index = syntext::index::Index::open(config.clone()).unwrap();
            black_box(index.stats());
            drop(index);
        });
    });

    // Rebuild into a directory that already has a manifest: measures the
    // repeat-build path (startup GC + threshold calibration reuse).
    group.bench_function("rebuild_in_place_300_files", |b| {
        let config = open_config(index_dir_300.path(), repo_300.path());
        b.iter(|| {
            let index = syntext::index::Index::build(config.clone()).unwrap();
            black_box(index.stats());
            drop(index);
        });
    });

    group.finish();
}

criterion_group!(benches, index_open_bench);
criterion_main!(benches);
