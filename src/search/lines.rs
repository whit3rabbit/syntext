//! Helpers for iterating line-oriented views over raw file bytes.

/// Visit each logical line in `content`.
///
/// Lines are split on `\n`. A preceding `\r` is trimmed so that `\r\n`
/// behaves like a single line ending. The callback receives:
/// - 1-based line number
/// - byte offset of the line start in the original content
/// - line bytes without trailing newline
///
/// # Mixed line endings (`\r\n` and `\n` in the same file)
///
/// Each line is handled independently: if a `\n` is preceded by `\r`, the
/// `\r` is excluded from the line slice but `line_start` always advances to
/// the byte after the `\n`. This means byte offsets are correct regardless
/// of whether any given line uses `\r\n` or bare `\n`. No per-line offset
/// drift occurs.
///
/// # Classic Mac `\r`-only files
///
/// Files using `\r` as the sole line separator (no `\n`) are treated as a
/// single line. This matches ripgrep behaviour, maintaining SC-004 correctness
/// parity. Matches in such files report `line_number: 1`.
pub(crate) fn for_each_line(content: &[u8], mut f: impl FnMut(u32, usize, &[u8])) {
    if content.is_empty() {
        return;
    }

    let mut line_num: u32 = 1;
    let mut line_start: usize = 0;

    for pos in memchr::memchr_iter(b'\n', content) {
        let line_end = if pos > line_start && content[pos - 1] == b'\r' {
            pos - 1
        } else {
            pos
        };
        f(line_num, line_start, &content[line_start..line_end]);
        line_num += 1;
        line_start = pos + 1;
    }

    if line_start < content.len() {
        f(line_num, line_start, &content[line_start..]);
    }
}
