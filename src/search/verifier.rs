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

use memchr::{memchr, memrchr, memchr_iter, memmem};
use regex::bytes::Regex;

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
    let mut matches = Vec::new();

    let mut last_line_start = usize::MAX;
    let mut current_line_num = 1;
    let mut last_newline_counted_up_to = 0;

    for match_start in finder.find_iter(content) {
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
            let newline_count = memchr_iter(b'\n', &content[last_newline_counted_up_to..line_start]).count();
            current_line_num += newline_count as u32;
            last_newline_counted_up_to = line_start;
        }

        matches.push(SearchMatch {
            path: path.to_path_buf(),
            line_number: current_line_num,
            line_content: content[line_start..line_content_end].to_vec(),
            byte_offset: match_start as u64,
            submatch_start: match_start - line_start,
            submatch_end: (match_start + pattern.len()) - line_start,
        });

        last_line_start = line_start;
    }

    matches
}

/// Verify a compiled regex against raw file bytes.
///
/// Returns one `SearchMatch` per matching line.
/// Binary content (null bytes) causes the file to be skipped entirely.
pub fn verify_regex(re: &Regex, path: &Path, content: &[u8]) -> Vec<SearchMatch> {
    if is_binary(content) {
        return Vec::new(); // skip binary files
    }
    let mut matches = Vec::new();

    let mut last_line_start = usize::MAX;
    let mut current_line_num = 1;
    let mut last_newline_counted_up_to = 0;

    for m in re.find_iter(content) {
        let match_start = m.start();
        let match_end = m.end();

        // 1. Line start is the byte after the last '\n' before match_start.
        //    Bounded by the watermark (a valid line start <= this line start,
        //    matches being in offset order); see verify_literal for the full
        //    rationale on why this avoids the quadratic full-prefix rescan.
        let from = last_newline_counted_up_to;
        let line_start = match memrchr(b'\n', &content[from..match_start]) {
            Some(pos) => from + pos + 1,
            None => from,
        };

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
        if match_end > line_content_end {
            continue;
        }

        // If this match is on the same line as the previous match, we skip it.
        if line_start == last_line_start {
            continue;
        }

        // 3. Count newlines between last_newline_counted_up_to and line_start
        if line_start > last_newline_counted_up_to {
            let newline_count = memchr_iter(b'\n', &content[last_newline_counted_up_to..line_start]).count();
            current_line_num += newline_count as u32;
            last_newline_counted_up_to = line_start;
        }

        matches.push(SearchMatch {
            path: path.to_path_buf(),
            line_number: current_line_num,
            line_content: content[line_start..line_content_end].to_vec(),
            byte_offset: match_start as u64,
            submatch_start: match_start - line_start,
            submatch_end: match_end - line_start,
        });

        last_line_start = line_start;
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
    fn literal_many_matches_across_and_clustered_on_lines() {
        // Guards the watermark-bounded line-start scan (#9): correct line numbers
        // and offsets when matches span many lines and cluster late in the file.
        // Build 500 leading no-match lines, then a run of match lines.
        let mut content = Vec::new();
        for _ in 0..500 {
            content.extend_from_slice(b"nomatch here\n");
        }
        // 3 match lines; only the first hit per line is reported.
        content.extend_from_slice(b"aa needle bb needle\n"); // line 501
        content.extend_from_slice(b"cc needle\n"); // line 502
        content.extend_from_slice(b"dd needle ee\n"); // line 503

        let matches = verify_literal("needle", Path::new("f"), &content);
        assert_eq!(matches.len(), 3, "one match reported per line");
        assert_eq!(matches[0].line_number, 501);
        assert_eq!(matches[0].line_content, b"aa needle bb needle");
        assert_eq!(matches[0].submatch_start, 3);
        assert_eq!(matches[1].line_number, 502);
        assert_eq!(matches[1].submatch_start, 3);
        assert_eq!(matches[2].line_number, 503);
        assert_eq!(matches[2].submatch_start, 3);
    }

    #[test]
    fn regex_line_numbers_correct_with_gaps() {
        let re = Regex::new("needle").unwrap();
        let content = b"a\nb\nc needle\nd\ne needle\n";
        let matches = verify_regex(&re, Path::new("f"), content);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_number, 3);
        assert_eq!(matches[1].line_number, 5);
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
