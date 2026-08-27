//! Shared NDJSON field/value helpers used by the `--json` and invert-match
//! renderers.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use crate::path_util::path_bytes;

pub(in crate::cli) fn json_data(bytes: &[u8]) -> serde_json::Value {
    if let Ok(text) = std::str::from_utf8(bytes) {
        serde_json::json!({ "text": text })
    } else {
        serde_json::json!({ "bytes": crate::base64::encode(bytes) })
    }
}

pub(in crate::cli) fn json_stats(
    elapsed: Duration,
    searches: usize,
    searches_with_match: usize,
    bytes_searched: usize,
    bytes_printed: usize,
    matched_lines: usize,
    matches: usize,
) -> serde_json::Value {
    serde_json::json!({
        "elapsed": json_elapsed(elapsed),
        "searches": searches,
        "searches_with_match": searches_with_match,
        "bytes_searched": bytes_searched,
        "bytes_printed": bytes_printed,
        "matched_lines": matched_lines,
        "matches": matches
    })
}

pub(in crate::cli) fn json_elapsed(elapsed: Duration) -> serde_json::Value {
    let human = if elapsed.is_zero() {
        "0s".to_string()
    } else if elapsed.as_secs() == 0 {
        format!("{:.6}s", elapsed.as_secs_f64())
    } else {
        format!("{:.3}s", elapsed.as_secs_f64())
    };
    serde_json::json!({
        "secs": elapsed.as_secs(),
        "nanos": elapsed.subsec_nanos(),
        "human": human
    })
}

/// The rendered form of a line: the `\r`-stripped slice plus its trailing
/// `\r` when the original content carries one. rg prints CRLF lines with the
/// `\r` intact; matching still runs on the stripped slice, so this is a
/// display-only widening.
pub(in crate::cli) fn rendered_line<'a>(
    content: &'a [u8],
    line_start: usize,
    stripped: &[u8],
) -> &'a [u8] {
    let base = line_start + stripped.len();
    match content.get(base) {
        Some(b'\r') => &content[line_start..base + 1],
        _ => &content[line_start..base],
    }
}

/// `line_is_unterminated_eof` is true when this line is the file's last line
/// and has no trailing `\n` at all (nothing follows it in the file).
pub(in crate::cli) fn json_submatches(
    re: &regex::bytes::Regex,
    line: &[u8],
    line_is_unterminated_eof: bool,
) -> Vec<serde_json::Value> {
    // Enumerate against the line with a bare trailing `\r` stripped: the
    // oracle's reference `rg` always runs with `--crlf` (see
    // tests/integration/oracle_helpers.rs), which folds a `\r` immediately
    // before a line terminator into that terminator, so it is never
    // searchable content (verified against rg 15.2.0 --crlf: `\s` on
    // "needle here\r\n" yields only the space, not the `\r`). The `\r` is
    // still kept in the displayed `lines` text, matching rg.
    let stripped = match line.split_last() {
        Some((b'\r', head)) => head,
        _ => line,
    };
    re.find_iter(stripped)
        .filter(|m| {
            // A zero-width match exactly at the end of a truly unterminated
            // final line is a phantom: with no `\n` anywhere after it, rg's
            // line-oriented searcher never runs the regex past the line's
            // last real byte. A `^...$` (`-x`) anchor otherwise applies its
            // multi_line+crlf end-of-line rule (also true at end-of-haystack)
            // and produces this same phantom on the isolated line slice.
            !(line_is_unterminated_eof && m.start() == m.end() && m.end() == stripped.len())
        })
        .map(|matched| {
            serde_json::json!({
                "match": json_data(&stripped[matched.start()..matched.end()]),
                "start": matched.start(),
                "end": matched.end()
            })
        })
        .collect()
}

pub(in crate::cli) fn json_line_message(
    message_type: &str,
    path: &Path,
    line_number: usize,
    absolute_offset: usize,
    line: &[u8],
    submatches: Vec<serde_json::Value>,
) -> String {
    let mut line_with_newline = line.to_vec();
    line_with_newline.push(b'\n');
    serde_json::json!({
        "type": message_type,
        "data": {
            "path": json_data(path_bytes(path).as_ref()),
            "lines": json_data(&line_with_newline),
            "line_number": line_number,
            "absolute_offset": absolute_offset,
            "submatches": submatches
        }
    })
    .to_string()
}

pub(in crate::cli) fn write_json_line(out: &mut dyn Write, line: &str) -> io::Result<usize> {
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    Ok(line.len() + 1)
}
