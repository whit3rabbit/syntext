#[path = "support/mod.rs"]
mod support;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use support::{build_index_for_repo, create_synthetic_repo};

fn query_latency_bench(c: &mut Criterion) {
    let repo = create_synthetic_repo(300);
    let (_index_dir, index) = build_index_for_repo(repo.path());
    let opts = syntext::SearchOptions::default();

    let mut group = c.benchmark_group("query_latency");
    group.sample_size(10);

    group.bench_function("literal_common", |b| {
        b.iter(|| {
            black_box(index.search("parse_query", &opts).unwrap().len());
        });
    });

    // #7: -l/-L path skips the per-matched-line byte copy. Comparing this to
    // `literal_common` (which populates line_content) on the same common token
    // is the before/after for the skip optimization.
    let skip_opts = syntext::SearchOptions {
        skip_line_content: true,
        ..Default::default()
    };
    group.bench_function("literal_common_skip_content", |b| {
        b.iter(|| {
            black_box(index.search("parse_query", &skip_opts).unwrap().len());
        });
    });

    group.bench_function("indexed_regex_rare", |b| {
        b.iter(|| {
            black_box(
                index
                    .search("(fn_parse_filter_query)+", &opts)
                    .unwrap()
                    .len(),
            );
        });
    });

    group.bench_function("full_scan_regex", |b| {
        b.iter(|| {
            black_box(
                index
                    .search("parse_query|process_batch", &opts)
                    .unwrap()
                    .len(),
            );
        });
    });

    group.finish();
}

criterion_group!(benches, query_latency_bench);
criterion_main!(benches);
