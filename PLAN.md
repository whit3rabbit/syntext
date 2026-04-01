# syntext v1.0 Release Plan

**Date**: 2026-03-29
**Current version**: 1.0.1
**Current state**: All phases complete. All release gates passed. Pending: git tags and crates.io publish.

---

## Release Checklist

- [x] All P0 bugs (B01-B08) fixed with regression tests
- [x] All P1 bugs either fixed or documented
- [x] `cargo test` passes
- [x] `cargo test --features symbols` passes
- [x] `cargo clippy` passes
- [x] No source file exceeds 400 lines (test files exempt)
- [x] Fuzz target: 10.1M executions, 0 crashes (2026-03-29)
- [x] Correctness harness: 16/16 tests pass
- [x] External corpus: Node.js v20.12.0 (40,812 files) validated
- [x] Benchmark presets: `node_runtime` validated, results in BENCHMARKS.md
- [x] `cargo publish --dry-run` succeeds
- [x] CHANGELOG.md created
- [x] README.md updated
- [x] All public APIs have doc comments
- [x] Known limitations documented
- [x] Cargo.toml metadata complete
- [x] Version bumped to `1.0.0`
- [ ] Git tag `v1.0.0-rc1` then `v1.0.0`
- [ ] CI release workflow (`release.yml`) tested with a pre-release tag
- [ ] GitHub Release with binaries (Linux amd64/arm64, macOS x86_64/arm64)
- [ ] `.deb` packages built and attached
- [ ] `cargo publish` to crates.io

---

## Deferred to v1.1+

| Item | Notes |
|------|-------|
| Crash recovery (T042) | Overlay generation files on disk, startup recovery. Current behavior: empty overlay on restart, `st update` to resync. |
| Background segment merge (T064) | Automatic compaction in background thread. `Index::compact()` handles the manual case. |
| Criterion benchmark suite (T061-T063) | External harness + presets are the canonical method for now. |
| Persistent/CoW overlay map | Replace `gram_index: HashMap` clone in `build_incremental_delta` with `im::HashMap` or `HashMap<u64, Arc<Vec<u32>>>`. Eliminates O(overlay) clone cost per `commit_batch` and the O(posting entries) sorted-order scan. Address together with the per-doc `.cloned()` carry-forward cost. |
| Windows Phase 2 security | `FILE_FLAG_OPEN_REPARSE_POINT` via `windows-sys` for native O_NOFOLLOW, ACL enforcement. Phase 1 (functional, `verify_fd_matches_stat` via volume+file_index) is complete. |
| FM-index | 10x slower construction, valid alternative indexing strategy. |
| Content-defined chunking | Block-level positional data for sub-file granularity. |
| Dual dictionary | Case-sensitive + case-insensitive (~2x dictionary size). |
| Overlapping trigrams | ~3.5x index size for non-aligned substring coverage. |
| Rate limiting on commit_batch | Accepted risk AR-002. |
