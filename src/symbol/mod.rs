//! Optional Tree-sitter symbol index (Tier 1 languages: Rust, Python, TypeScript, Go, Java).
//!
//! Symbols are stored in a SQLite database alongside the RPLX segments and
//! queried separately from the n-gram content index. Implemented in Phase 7 (US4).

pub mod extractor;
