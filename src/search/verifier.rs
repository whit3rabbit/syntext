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
use regex::bytes::Regex;

use crate::index::is_binary;
use crate::SearchMatch;

use super::lines::for_each_line;

/// Verify a literal pattern against raw file bytes using `memchr::memmem`.
///
/// Case-sensitive. Returns one `SearchMatch` per matching line.
/// Binary content (null bytes) causes the file to be skipped entirely.
pub fn verify_literal(pattern: &str, path: &Path, content: &[u8]) -> Vec<SearchMatch> {
    if is_binary(content) {
        return Vec::new(); // skip binary files
    }
    let finder = memmem::Finder::new(pattern.as_bytes());
    collect_line_matches(path, content, |line| {
        finder
            .find(line)
            .map(|start| (start, start + pattern.len()))
    })
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
        re.find(line).map(|m| (m.start(), m.end()))
    })
}

/// Iterate `content` line by line, calling `predicate` on each line's bytes.
/// Returns `SearchMatch` for every line where `predicate` returns the start
/// offset of the first match within that line.
fn collect_line_matches(
    path: &Path,
    content: &[u8],
    mut predicate: impl FnMut(&[u8]) -> Option<(usize, usize)>,
) -> Vec<SearchMatch> {
    let mut matches = Vec::new();
    for_each_line(content, |line_num, line_start, line| {
        if let Some((match_start, match_end)) = predicate(line) {
            matches.push(SearchMatch {
                path: path.to_path_buf(),
                line_number: line_num,
                line_content: line.to_vec(),
                byte_offset: (line_start + match_start) as u64,
                submatch_start: match_start,
                submatch_end: match_end,
            });
        }
    });

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
        assert_eq!(matches[0].submatch_start, 7);
        assert_eq!(matches[0].submatch_end, 13);
    }

    #[test]
    fn regex_reports_match_start_offset() {
        let re = Regex::new("needle").unwrap();
        let matches = verify_regex(&re, Path::new("file.txt"), b"prefix needle suffix\n");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].byte_offset, 7);
        assert_eq!(matches[0].submatch_start, 7);
        assert_eq!(matches[0].submatch_end, 13);
    }

    #[test]
    fn crlf_offsets_include_line_break_bytes_before_match() {
        let matches = verify_literal("needle", Path::new("file.txt"), b"one\r\ntwo needle\r\n");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 2);
        assert_eq!(matches[0].byte_offset, 9);
        assert_eq!(matches[0].line_content, b"two needle");
    }

    #[test]
    fn regex_matches_invalid_utf8_line_bytes() {
        let re = Regex::new(r"(?-u)\xFF").unwrap();
        let matches = verify_regex(&re, Path::new("file.bin"), b"prefix\xFFsuffix\n");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_content, b"prefix\xFFsuffix");
        assert_eq!(matches[0].submatch_start, 6);
        assert_eq!(matches[0].submatch_end, 7);
    }
}
