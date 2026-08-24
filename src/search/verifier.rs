//! Tiered verifier: confirms index candidates against actual file bytes.
//!
//! Two tiers:
//! - **Literal**: `memchr::memmem` for case-sensitive literal patterns. Fast path.
//! - **Regex**: compiled `regex::Regex` for everything else (regex patterns and
//!   case-insensitive literals). Correct for all inputs.
//!
//! Both tiers operate line-by-line: a file is split at `\n` boundaries, and each
//! line is checked independently. This matches ripgrep's default behavior.

use std::path::Path;

use memchr::{memchr, memchr_iter, memmem, memrchr};
use regex::bytes::Regex;

use crate::index::is_binary;
use crate::SearchMatch;

/// Verify a literal pattern against raw file bytes using `memchr::memmem`.
///
/// Case-sensitive. Returns one `SearchMatch` per matching line.
/// Binary content (null bytes) causes the file to be skipped entirely.
///
/// When `skip_line_content` is true, `line_content` is left empty (no per-line
/// byte copy) for callers that only need which files/lines matched (`-l`/`-L`).
pub fn verify_literal(
    pattern: &str,
    path: &Path,
    content: &[u8],
    skip_line_content: bool,
) -> Vec<SearchMatch> {
    if is_binary(content) {
        return Vec::new(); // skip binary files
    }
    let finder = memmem::Finder::new(pattern.as_bytes());
    let mut matches = Vec::new();

    let mut last_line_start = usize::MAX;
    let mut current_line_num = 1;
    let mut last_newline_counted_up_to = 0;
    let mut current_line_end = 0;

    for match_start in finder.find_iter(content) {
        if match_start < current_line_end {
            continue;
        }

        // Locate line boundaries around hits
        // 1. Line start is the byte after the last '\n' before match_start.
        //    Bound the backward scan to `last_newline_counted_up_to`, which is
        //    always 0 or a byte-after-newline (a valid line start) and, because
        //    matches arrive in increasing offset order, is <= this match's line
        //    start. Scanning only `[watermark..match_start]` removes the
        //    O(matches * file_size) full-prefix rescan.
        let from = last_newline_counted_up_to;
        let line_start = match memrchr(b'\n', &content[from..match_start]) {
            Some(pos) => from + pos + 1,
            None => from,
        };

        // If this match is on the same line as the previous match, we skip it
        // because we only return the first match per line.
        if line_start == last_line_start {
            continue;
        }

        // 2. Line end is the first '\n' at or after match_start (or end of file)
        let next_newline = memchr(b'\n', &content[match_start..]);
        let line_end = match next_newline {
            Some(pos) => match_start + pos,
            None => content.len(),
        };

        // Trim trailing '\r' if present
        let line_content_end = if line_end > line_start && content[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };

        // 3. Count newlines between last_newline_counted_up_to and line_start
        if line_start > last_newline_counted_up_to {
            let newline_count =
                memchr_iter(b'\n', &content[last_newline_counted_up_to..line_start]).count();
            current_line_num += newline_count as u32;
            last_newline_counted_up_to = line_start;
        }

        matches.push(SearchMatch {
            path: path.to_path_buf(),
            line_number: current_line_num,
            line_content: if skip_line_content {
                Vec::new()
            } else {
                // Rendered bytes run through line_end so a CRLF line keeps
                // its `\r` (rg prints the raw line); spans stay clamped short
                // of it below.
                content[line_start..line_end].to_vec()
            },
            byte_offset: match_start as u64,
            submatch_start: match_start - line_start,
            // Clamp to line_content_end so spans never cover the rendered
            // `\r`: a pattern ending in '\r' can match at end-of-line, and
            // the raw `match_start + pattern.len()` would run one byte past
            // the matchable content.
            submatch_end: (match_start + pattern.len()).min(line_content_end) - line_start,
        });

        last_line_start = line_start;
        current_line_end = line_end;
    }

    matches
}

/// Verify a compiled regex against raw file bytes.
///
/// Returns one `SearchMatch` per matching line.
/// Binary content (null bytes) causes the file to be skipped entirely.
///
/// When `skip_line_content` is true, `line_content` is left empty (see
/// [`verify_literal`]).
pub fn verify_regex(
    re: &Regex,
    path: &Path,
    content: &[u8],
    skip_line_content: bool,
) -> Vec<SearchMatch> {
    if is_binary(content) {
        return Vec::new(); // skip binary files
    }
    let mut matches = Vec::new();

    let mut last_line_start = usize::MAX;
    let mut current_line_num = 1;
    let mut last_newline_counted_up_to = 0;
    let mut current_line_end = 0;

    for m in re.find_iter(content) {
        let match_start = m.start();
        let match_end = m.end();

        if match_start < current_line_end {
            continue;
        }

        // 1. Line start is the byte after the last '\n' before match_start.
        //    Bounded by the watermark (a valid line start <= this line start,
        //    matches being in offset order); see verify_literal for the full
        //    rationale on why this avoids the quadratic full-prefix rescan.
        let from = last_newline_counted_up_to;
        let line_start = match memrchr(b'\n', &content[from..match_start]) {
            Some(pos) => from + pos + 1,
            None => from,
        };

        // A zero-width match at end-of-content sits after the final newline:
        // no line exists there. rg prints no phantom trailing empty line for
        // patterns like `x|` or an empty -f/--file pattern line.
        if line_start >= content.len() {
            continue;
        }

        // 2. Line end is the first '\n' at or after match_start (or end of file)
        let next_newline = memchr(b'\n', &content[match_start..]);
        let line_end = match next_newline {
            Some(pos) => match_start + pos,
            None => content.len(),
        };

        // Trim trailing '\r' if present
        let line_content_end = if line_end > line_start && content[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };

        // If the match spans across a newline, it is invalid (matches must be line-by-line).
        if match_end > line_end {
            continue;
        }

        // If this match is on the same line as the previous match, we skip it.
        if line_start == last_line_start {
            continue;
        }

        // 3. Count newlines between last_newline_counted_up_to and line_start
        if line_start > last_newline_counted_up_to {
            let newline_count =
                memchr_iter(b'\n', &content[last_newline_counted_up_to..line_start]).count();
            current_line_num += newline_count as u32;
            last_newline_counted_up_to = line_start;
        }

        matches.push(SearchMatch {
            path: path.to_path_buf(),
            line_number: current_line_num,
            line_content: if skip_line_content {
                Vec::new()
            } else {
                // Rendered bytes keep a CRLF line's `\r` (rg parity); spans
                // are clamped short of it.
                content[line_start..line_end].to_vec()
            },
            byte_offset: match_start as u64,
            submatch_start: match_start - line_start,
            submatch_end: match_end.min(line_content_end) - line_start,
        });

        last_line_start = line_start;
        current_line_end = line_end;
    }

    matches
}

/// Match every line of the file (for empty pattern searches).
pub fn verify_empty(path: &Path, content: &[u8], skip_line_content: bool) -> Vec<SearchMatch> {
    if is_binary(content) {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let mut line_start = 0;
    let mut line_num = 1;

    for pos in memchr_iter(b'\n', content) {
        // Rendered bytes include a CRLF line's `\r` (rg parity).
        matches.push(SearchMatch {
            path: path.to_path_buf(),
            line_number: line_num,
            line_content: if skip_line_content {
                Vec::new()
            } else {
                content[line_start..pos].to_vec()
            },
            byte_offset: line_start as u64,
            submatch_start: 0,
            submatch_end: 0,
        });
        line_start = pos + 1;
        line_num += 1;
    }

    // Only a real unterminated final line remains: when content ends with
    // '\n' the loop above already emitted every line, and position len is not
    // a line (same phantom-trailing-empty-line rule as verify_regex).
    if line_start < content.len() {
        matches.push(SearchMatch {
            path: path.to_path_buf(),
            line_number: line_num,
            line_content: if skip_line_content {
                Vec::new()
            } else {
                content[line_start..].to_vec()
            },
            byte_offset: line_start as u64,
            submatch_start: 0,
            submatch_end: 0,
        });
    }

    matches
}

#[cfg(test)]
#[path = "verifier_tests.rs"]
mod tests;
