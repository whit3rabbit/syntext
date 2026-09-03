//! `--type-list`: print ripgrep's file-type definitions.
//!
//! Split out of `manage.rs` to keep that file under the 400-line quality gate.

use std::io::{self, Write};

/// Print supported file types in ripgrep-compatible format.
pub(super) fn cmd_type_list() -> i32 {
    use ignore::types::TypesBuilder;
    let mut builder = TypesBuilder::new();
    builder.add_defaults();
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    for def in builder.definitions() {
        let globs: Vec<String> = def.globs().iter().map(|g| g.to_string()).collect();
        entries.push((def.name().to_string(), globs));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (name, globs) in &entries {
        let joined = globs.join(", ");
        if writeln!(out, "{name}: {joined}").is_err() {
            return 0; // broken pipe
        }
    }
    0
}
