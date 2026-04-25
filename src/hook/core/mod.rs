//! Shared hook primitives used by vendor installers and protocols.

pub mod files;
pub mod instructions;
pub mod json;
pub mod rewrite;
pub(crate) mod shell;

#[cfg(test)]
mod rewrite_tests;
