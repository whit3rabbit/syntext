//! Shared prompt/rules-file patch helpers.

use std::fs;
use std::path::Path;

use super::files;

pub(crate) const AWARENESS_FILE: &str = "SYNTEXT.md";
pub(crate) const AWARENESS_REF: &str = "@SYNTEXT.md";

/// The injected guidance body, as a literal.
///
/// A macro rather than a `const` so `concat!` can splice it into `AWARENESS`
/// at compile time. The awareness file and the marker-guarded rules block must
/// carry identical text, and the only way to guarantee that is to write it
/// once.
///
/// Two things this text deliberately does not say. It does not tell the agent
/// to run `st update` after edits, because search already refreshes from git
/// on every call (`crate::cli::catchup`), so the old instruction cost a tool
/// call per edit for nothing. And it now tells the agent to bound its own
/// output, because an unbounded match on a common token is the failure mode
/// that actually burns an agent's context window.
macro_rules! guidance_body {
    () => {
        concat!(
            "Use `st` instead of `rg` or `grep` when `.syntext/` exists. ",
            "`st` accepts ripgrep's flags and prints ripgrep's output.\n",
            "Before the first search in a repo: `test -d .syntext || st index`.\n",
            "Do not run `st update` after edits. Search refreshes from git on every call. ",
            "Run `st update` only when `st` prints \"files behind\".\n",
            "Bound output with `--max-results N`, `-l`, `-c`, `-m N`, `-C N`, `-g GLOB`, ",
            "or `-t TYPE`. ",
            "Treat a returned line with context as already-read evidence. ",
            "Read the file only when more is needed.\n",
            "Use native `rg` only for paths outside the indexed repo root.\n",
        )
    };
}

/// Guidance body shared by every install surface. See [`guidance_body`].
pub(crate) const GUIDANCE: &str = guidance_body!();

pub(crate) const AWARENESS: &str = concat!("# Syntext\n\n", guidance_body!());

pub(crate) fn marker_start(id: &str) -> String {
    format!("<!-- syntext-agent:{id}:start -->")
}

fn marker_end(id: &str) -> String {
    format!("<!-- syntext-agent:{id}:end -->")
}

pub(crate) fn syntext_block(id: &str, heading: &str) -> String {
    format!(
        "{}\n## {heading}\n\n{GUIDANCE}{}\n",
        marker_start(id),
        marker_end(id)
    )
}

pub(crate) fn ensure_block(path: &Path, id: &str, block: &str) -> Result<bool, String> {
    let existing = read_optional(path)?;
    if existing.contains(&marker_start(id)) {
        return Ok(false);
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(block);
    files::write_text_if_changed(path, &next)
}

pub(crate) fn remove_block(path: &Path, id: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let existing = fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    let start_marker = marker_start(id);
    let end_marker = marker_end(id);
    let Some(start) = existing.find(&start_marker) else {
        return Ok(false);
    };
    let Some(end_start) = existing[start..].find(&end_marker) else {
        return Ok(false);
    };
    let end = start + end_start + end_marker.len();
    let mut next = String::new();
    next.push_str(existing[..start].trim_end());
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(existing[end..].trim_start_matches(['\r', '\n']));
    files::write_text_if_changed(path, &next)
}

pub(crate) fn ensure_line(path: &Path, line: &str) -> Result<bool, String> {
    let existing = read_optional(path)?;
    if existing
        .lines()
        .any(|existing_line| existing_line.trim() == line)
    {
        return Ok(false);
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(line);
    next.push('\n');
    files::write_text_if_changed(path, &next)
}

pub(crate) fn remove_line(path: &Path, line: &str) -> Result<bool, String> {
    if !path.exists() {
        return Ok(false);
    }
    let existing = fs::read_to_string(path)
        .map_err(|err| format!("st: failed to read {}: {err}", path.display()))?;
    let mut removed = false;
    let mut lines = Vec::new();
    for existing_line in existing.lines() {
        if existing_line.trim() == line {
            removed = true;
        } else {
            lines.push(existing_line);
        }
    }
    if !removed {
        return Ok(false);
    }
    let mut next = lines.join("\n");
    if !next.is_empty() {
        next.push('\n');
    }
    files::write_text_if_changed(path, &next)
}

fn read_optional(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).map_err(|err| format!("st: failed to read {}: {err}", path.display()))
}
