//! Agent hook integration support.

/// Core hook installation primitives and rewriting logic.
pub mod core;
/// IPC/protocol hooks (e.g. wrapper scripts/daemons).
pub mod protocols;
/// Vendor-specific rules and integrations (Claude, Cursor, Gemini, etc.).
pub mod vendors;
