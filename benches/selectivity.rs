use criterion::{criterion_group, criterion_main, Criterion};

fn selectivity_bench(_c: &mut Criterion) {
    // Implemented in Phase 9
}

criterion_group!(benches, selectivity_bench);
criterion_main!(benches);
