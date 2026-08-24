use super::*;

#[test]
fn literal_reports_match_start_offset() {
    let matches = verify_literal(
        "needle",
        Path::new("file.txt"),
        b"prefix needle suffix\n",
        false,
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].byte_offset, 7);
    assert_eq!(matches[0].submatch_start, 7);
    assert_eq!(matches[0].submatch_end, 13);
}

#[test]
fn regex_reports_match_start_offset() {
    let re = Regex::new("needle").unwrap();
    let matches = verify_regex(&re, Path::new("file.txt"), b"prefix needle suffix\n", false);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].byte_offset, 7);
    assert_eq!(matches[0].submatch_start, 7);
    assert_eq!(matches[0].submatch_end, 13);
}

#[test]
fn literal_pattern_ending_in_cr_clamps_submatch_end() {
    // Pattern ends in '\r' and matches right before the '\n'. line_content
    // keeps the rendered '\r' (rg parity), but the match span must clamp to
    // the matchable content so it never covers that byte.
    let matches = verify_literal("abc\r", Path::new("f"), b"abc\r\n", false);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line_content, b"abc\r");
    assert!(
        matches[0].submatch_end < matches[0].line_content.len(),
        "submatch_end {} must stay short of the rendered \\r at {}",
        matches[0].submatch_end,
        matches[0].line_content.len()
    );
    // The clamped span is still sliceable without panicking.
    let _ = &matches[0].line_content[matches[0].submatch_start..matches[0].submatch_end];
}

#[test]
fn crlf_offsets_include_line_break_bytes_before_match() {
    let matches = verify_literal(
        "needle",
        Path::new("file.txt"),
        b"one\r\ntwo needle\r\n",
        false,
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line_number, 2);
    assert_eq!(matches[0].byte_offset, 9);
    assert_eq!(matches[0].line_content, b"two needle\r");
}

#[test]
fn literal_many_matches_across_and_clustered_on_lines() {
    // Ensure correct line numbers and offsets when matches span many lines
    // and cluster late in the file, verifying the line-start scan behavior.
    // Build 500 leading no-match lines, then a run of match lines.
    let mut content = Vec::new();
    for _ in 0..500 {
        content.extend_from_slice(b"nomatch here\n");
    }
    // 3 match lines; only the first hit per line is reported.
    content.extend_from_slice(b"aa needle bb needle\n"); // line 501
    content.extend_from_slice(b"cc needle\n"); // line 502
    content.extend_from_slice(b"dd needle ee\n"); // line 503

    let matches = verify_literal("needle", Path::new("f"), &content, false);
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
    let matches = verify_regex(&re, Path::new("f"), content, false);
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].line_number, 3);
    assert_eq!(matches[1].line_number, 5);
}

#[test]
fn skip_line_content_leaves_content_empty_but_keeps_offsets() {
    // -l/-L path: line_content is skipped, but line numbers and match
    // offsets stay correct.
    let lit = verify_literal("needle", Path::new("f"), b"a\nx needle y\n", true);
    assert_eq!(lit.len(), 1);
    assert!(lit[0].line_content.is_empty(), "content skipped");
    assert_eq!(lit[0].line_number, 2);
    assert_eq!(lit[0].submatch_start, 2);
    assert_eq!(lit[0].submatch_end, 8);

    let re = Regex::new("needle").unwrap();
    let rgx = verify_regex(&re, Path::new("f"), b"a\nx needle y\n", true);
    assert_eq!(rgx.len(), 1);
    assert!(rgx[0].line_content.is_empty());
    assert_eq!(rgx[0].line_number, 2);
    assert_eq!(rgx[0].submatch_start, 2);
}

#[test]
fn regex_matches_invalid_utf8_line_bytes() {
    let re = Regex::new(r"(?-u)\xFF").unwrap();
    let matches = verify_regex(&re, Path::new("file.bin"), b"prefix\xFFsuffix\n", false);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line_content, b"prefix\xFFsuffix");
    assert_eq!(matches[0].submatch_start, 6);
    assert_eq!(matches[0].submatch_end, 7);
}

#[test]
fn regex_pattern_ending_in_cr_clamps_submatch_end() {
    let re = Regex::new("abc\r").unwrap();
    let matches = verify_regex(&re, Path::new("f"), b"abc\r\n", false);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line_content, b"abc\r");
    assert!(
        matches[0].submatch_end < matches[0].line_content.len(),
        "submatch_end {} must stay short of the rendered \\r at {}",
        matches[0].submatch_end,
        matches[0].line_content.len()
    );
    let _ = &matches[0].line_content[matches[0].submatch_start..matches[0].submatch_end];
}

#[test]
fn empty_pattern_matches_all_lines() {
    let content = b"line one\nline two\r\nline three";
    let matches = verify_empty(Path::new("f"), content, false);
    assert_eq!(matches.len(), 3);
    assert_eq!(matches[0].line_number, 1);
    assert_eq!(matches[0].line_content, b"line one");
    assert_eq!(matches[1].line_number, 2);
    assert_eq!(matches[1].line_content, b"line two\r");
    assert_eq!(matches[2].line_number, 3);
    assert_eq!(matches[2].line_content, b"line three");
}

#[test]
fn regex_zero_width_at_end_of_content_is_not_a_phantom_line() {
// `x|` matches zero-width at end-of-content, which sits after the final
// newline; no line exists there (rg prints no trailing empty line). This
// is reachable via an empty -f/--file pattern line ORed into the query.
let re = regex::bytes::Regex::new("x|").unwrap();
let matches = verify_regex(&re, Path::new("f"), b"alpha\nbeta\n", false);
assert_eq!(matches.len(), 2, "{matches:?}");
}

#[test]
fn empty_pattern_does_not_emit_phantom_trailing_line() {
let matches = verify_empty(Path::new("f"), b"alpha\nbeta\n", false);
assert_eq!(matches.len(), 2, "{matches:?}");
}

#[test]
fn crlf_output_regex_semantics_on_r_included_lines() {
    // Pins the multi_line+crlf semantics the rendering path relies on
    // (compile_output_regex builds exactly this): \r\n, a lone \r, and \n
    // are all line boundaries, so a $-anchored pattern behaves the same on
    // a \r-included line slice as on the stripped one, and `.` does not
    // consume the \r. Rendering therefore only needs to widen the displayed
    // bytes (`rendered_line`); matching semantics are unchanged.
    let b = |p: &str| {
        regex::bytes::RegexBuilder::new(p)
            .crlf(true)
            .multi_line(true)
            .build()
            .unwrap()
    };
    assert!(
        b("parse$").is_match(b"parse\r"),
        "$ anchors before a lone trailing \\r"
    );
    assert!(b("parse$").is_match(b"parse\r\n"), "$ anchors before \\r\\n");
    assert!(
        !b("parse.$").is_match(b"parse\r"),
        "`.` must not consume the \\r"
    );
    assert!(b("parse\\r$").is_match(b"parse\r"), "an explicit \\r is still matchable");
}
