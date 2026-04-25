//! Shared prompt/rules-file patch helpers.

use std::fs;
use std::path::Path;

use super::files;

pub(crate) const AWARENESS_FILE: &str = "SYNTEXT.md";
pub(crate) const AWARENESS_REF: &str = "@SYNTEXT.md";

pub(crate) const AWARENESS: &str = r#"# Syntext

Use `st` instead of `rg` or `grep` when `.syntext/` exists.
Before the first search in a repo, run `test -d .syntext || st index`.
After file edits, run `st update` before relying on search results.
"#;

pub(crate) fn marker_start(id: &str) -> String {
    format!("<!-- syntext-agent:{id}:start -->")
}

fn marker_end(id: &str) -> String {
    format!("<!-- syntext-agent:{id}:end -->")
}

pub(crate) fn syntext_block(id: &str, heading: &str) -> String {
    format!(
        "{}\n## {heading}\n\nUse `st` instead of `rg` or `grep` when `.syntext/` exists.\nBefore the first search in a repo, run `test -d .syntext || st index`.\nAfter file edits, run `st update` before relying on search results.\n{}\n",
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
