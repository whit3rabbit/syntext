//! Shared hook primitives used by vendor installers and protocols.

/// Helper utilities for finding, writing, and resolving hook files.
pub mod files;
/// Setup/usage instruction generators for hook setups.
pub mod instructions;
/// JSON configuration format and serialization utilities.
pub mod json;
/// Configuration file rewriting utilities.
pub mod rewrite;
pub(crate) mod shell;

#[cfg(test)]
mod rewrite_tests;
