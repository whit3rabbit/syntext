#[path = "support/mod.rs"]
mod support;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use support::{build_index_for_repo, create_synthetic_repo};

fn selectivity_bench(c: &mut Criterion) {
    let repo = create_synthetic_repo(300);
    let (_index_dir, index) = build_index_for_repo(repo.path());
    let opts = syntext::SearchOptions::default();

    let mut group = c.benchmark_group("selectivity");
    group.sample_size(10);

    group.bench_function("literal_no_match", |b| {
        b.iter(|| {
            black_box(
                index
                    .search("xyzzy_no_match_sentinel_42", &opts)
                    .unwrap()
                    .len(),
            );
        });
    });

    group.bench_function("indexed_regex_selective", |b| {
        b.iter(|| {
            black_box(
                index
                    .search("(fn_parse_filter_query)+", &opts)
                    .unwrap()
                    .len(),
            );
        });
    });

    group.bench_function("literal_broad", |b| {
        b.iter(|| {
            black_box(index.search("detect_language", &opts).unwrap().len());
        });
    });

    group.finish();
}

criterion_group!(benches, selectivity_bench);
criterion_main!(benches);
