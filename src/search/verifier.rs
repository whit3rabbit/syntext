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

use memchr::memmem;
use regex::Regex;

use crate::index::is_binary;
use crate::SearchMatch;

/// Verify a literal pattern against raw file bytes using `memchr::memmem`.
///
/// Case-sensitive. Returns one `SearchMatch` per matching line.
/// Binary content (null bytes) causes the file to be skipped entirely.
pub fn verify_literal(pattern: &str, path: &Path, content: &[u8]) -> Vec<SearchMatch> {
    if is_binary(content) {
        return Vec::new(); // skip binary files
    }
    let finder = memmem::Finder::new(pattern.as_bytes());
    collect_line_matches(path, content, |line| finder.find(line))
}

/// Verify a compiled regex against raw file bytes.
///
/// Returns one `SearchMatch` per matching line.
/// Binary content (null bytes) causes the file to be skipped entirely.
pub fn verify_regex(re: &Regex, path: &Path, content: &[u8]) -> Vec<SearchMatch> {
    if is_binary(content) {
        return Vec::new(); // skip binary files
    }
    collect_line_matches(path, content, |line| {
        // Convert bytes to str; skip lines that are not valid UTF-8.
        if let Ok(s) = std::str::from_utf8(line) {
            re.find(s).map(|m| m.start())
        } else {
            None
        }
    })
}

/// Iterate `content` line by line, calling `predicate` on each line's bytes.
/// Returns `SearchMatch` for every line where `predicate` returns the start
/// offset of the first match within that line.
fn collect_line_matches(
    path: &Path,
    content: &[u8],
    mut predicate: impl FnMut(&[u8]) -> Option<usize>,
) -> Vec<SearchMatch> {
    let mut matches = Vec::new();
    let mut line_num: u32 = 1;
    let mut line_start: usize = 0;

    for i in 0..=content.len() {
        let is_end = i == content.len();
        let is_newline = !is_end && content[i] == b'\n';

        if is_newline || is_end {
            let line_end = if is_newline && i > line_start && content[i - 1] == b'\r' {
                i - 1 // strip \r from \r\n
            } else {
                i
            };
            let line = &content[line_start..line_end];

            if let Some(match_start) = predicate(line) {
                matches.push(SearchMatch {
                    path: path.to_path_buf(),
                    line_number: line_num,
                    line_content: String::from_utf8_lossy(line).into_owned(),
                    byte_offset: (line_start + match_start) as u64,
                });
            }

            line_num += 1;
            line_start = i + 1;
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_reports_match_start_offset() {
        let matches = verify_literal("needle", Path::new("file.txt"), b"prefix needle suffix\n");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].byte_offset, 7);
    }

    #[test]
    fn regex_reports_match_start_offset() {
        let re = Regex::new("needle").unwrap();
        let matches = verify_regex(&re, Path::new("file.txt"), b"prefix needle suffix\n");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].byte_offset, 7);
    }

    #[test]
    fn crlf_offsets_include_line_break_bytes_before_match() {
        let matches = verify_literal("needle", Path::new("file.txt"), b"one\r\ntwo needle\r\n");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 2);
        assert_eq!(matches[0].byte_offset, 9);
        assert_eq!(matches[0].line_content, "two needle");
    }
}
