use criterion::{criterion_group, criterion_main, Criterion};

fn query_latency_bench(_c: &mut Criterion) {
    // Implemented in Phase 9
}

criterion_group!(benches, query_latency_bench);
criterion_main!(benches);
