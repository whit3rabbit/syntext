<!--
  Sync Impact Report
  ===================
  Version change: N/A -> 1.0.0 (initial adoption)
  Added principles:
    - I. Security First
    - II. Speed-Optimized Development
    - III. Research Before Action
    - IV. Test and Document Everything
    - V. Enforce Module Size Limits
  Added sections:
    - Development Workflow
    - Quality Gates
    - Governance
  Templates requiring updates:
    - .specify/templates/plan-template.md: constitution check section is
      generic placeholder, compatible as-is
    - .specify/templates/spec-template.md: compatible as-is
    - .specify/templates/tasks-template.md: compatible as-is
  Follow-up TODOs: none
-->

# Ripline Constitution

## Core Principles

### I. Security First

All design and implementation decisions MUST prioritize security
before performance. Specifically:

- Input validation and sanitization at every system boundary
  (user queries, file paths, regex patterns).
- No unsafe Rust blocks without documented justification and
  review. Prefer safe abstractions.
- Regex verification MUST use a linear-time engine by default
  to prevent catastrophic backtracking (ReDoS).
- Dependencies MUST be audited before adoption. Minimize the
  attack surface: fewer crates, fewer risks.
- Memory-mapped files MUST be handled defensively (bounds
  checks, error recovery on corrupt data).

**Rationale**: A code search engine processes arbitrary
repository content and user-supplied patterns. A single
vulnerability (path traversal, ReDoS, buffer overread) can
compromise the host system or stall agent workflows.

### II. Speed-Optimized Development

Every change MUST consider whether a faster approach exists
for the operation it implements or the workflow it supports.

- Profile before optimizing: measure with benchmarks, not
  intuition.
- Prefer algorithms that reduce work (sparse n-grams, posting
  list intersection order) over micro-optimizations.
- Index layout MUST be mmap-friendly for zero-copy reads.
- Candidate set minimization (fewer posting lists loaded, fewer
  documents verified) is the primary speed lever.
- Agent-facing latency targets: sub-50ms warm queries, sub-5s
  full index rebuild for typical repositories.

**Rationale**: Ripline exists to break the linear scan cost
curve. If a change does not move toward faster search or faster
development iteration, it needs justification.

### III. Research Before Action

No implementation work MUST begin without prior research into
existing solutions, tradeoffs, and prior art.

- Before adding a dependency: check alternatives, license,
  maintenance status, and security history.
- Before designing a subsystem: review how Zoekt, Tantivy,
  ripgrep, or GitHub Blackbird solve the same problem.
- Before choosing a data structure: document time/space
  complexity and expected access patterns.
- Research output MUST be captured in spec or plan documents,
  not discarded after a conversation.

**Rationale**: Ripline operates in a well-studied domain.
Ignoring prior art leads to reinventing known-bad solutions.
Research is cheaper than rework.

### IV. Test and Document Everything

Every feature and bug fix MUST include tests. Every public
API and non-obvious internal decision MUST be documented.

- Unit tests for isolated logic (tokenizers, posting list
  operations, query planning).
- Integration tests for cross-component flows (index build
  then query, incremental update then verify).
- Benchmark tests for performance-critical paths (query
  latency, index throughput).
- Documentation lives next to code: doc comments for public
  APIs, markdown for architecture decisions.
- Test files are exempt from the 400-line refactoring rule
  (Principle V).

**Rationale**: Tests are the executable specification. Without
them, refactors break silently and regressions hide. Without
docs, onboarding cost grows with every module.

### V. Enforce Module Size Limits

Any source file exceeding 400 lines (excluding test files)
MUST be refactored before merging.

- Split by responsibility: one concern per module.
- Extract shared logic into well-named, focused modules.
- Test files (`tests/`, `*_test.rs`, `benches/`) are exempt
  from this limit.
- If a file genuinely cannot be split without harming
  readability, document the exception in a code comment at
  the top of the file with rationale.

**Rationale**: Large files signal tangled responsibilities.
Enforcing a line budget forces cleaner module boundaries,
which in turn makes the codebase faster to search, read,
and modify.

## Development Workflow

- **Branch strategy**: feature branches off main, squash merge
  after review.
- **Commit discipline**: atomic commits, each passing tests.
- **Review checklist**: security audit, test coverage, doc
  coverage, line count check.
- **Dependency changes**: require explicit approval before
  adding or upgrading production dependencies.

## Quality Gates

All pull requests MUST pass these gates before merge:

1. `cargo test` passes with no failures.
2. `cargo clippy` reports no warnings.
3. No source file exceeds 400 lines (test files exempt).
4. New public APIs have doc comments.
5. Performance-sensitive changes include benchmark results
   (before/after).
6. No new `unsafe` blocks without documented justification.

## Governance

This constitution supersedes ad-hoc practices. Amendments
require:

1. A written proposal describing the change and rationale.
2. Review and approval before adoption.
3. Version bump following semantic versioning:
   - MAJOR: principle removal or incompatible redefinition.
   - MINOR: new principle or materially expanded guidance.
   - PATCH: clarifications, wording, typo fixes.
4. Update of `LAST_AMENDED_DATE` on every change.
5. Propagation check across dependent templates.

All code reviews MUST verify compliance with these principles.
Complexity that violates a principle MUST be justified in the
plan document's Complexity Tracking table.

**Version**: 1.0.0 | **Ratified**: 2026-03-25 | **Last Amended**: 2026-03-25
