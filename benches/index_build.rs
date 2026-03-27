use criterion::{criterion_group, criterion_main, Criterion};

fn index_build_bench(_c: &mut Criterion) {
    // Implemented in Phase 9
}

criterion_group!(benches, index_build_bench);
criterion_main!(benches);
