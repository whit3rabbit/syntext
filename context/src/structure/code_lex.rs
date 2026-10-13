//! Minimal lexer computing brace depth per line for brace-family languages.
//!
//! It only needs to tell structural braces from braces inside strings and
//! comments, so it tracks string and comment state and nothing else. It is
//! deliberately forgiving: a mis-lexed line can at worst merge or split two
//! passages, never lose text.

use super::code::LineInfo;
use super::lang::Lang;
use super::lines::Lines;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    BlockComment,
    Double,
    Single,
    Backtick,
    Triple,
    /// Rust raw string with this many `#`s.
    Raw(usize),
}

pub(super) fn brace_infos(lines: &Lines<'_>, lang: Lang) -> Vec<LineInfo> {
    let comments = lang.line_comments();
    let block = lang.has_block_comments();
    let mut state = State::Code;
    let mut depth: u32 = 0;
    let mut out = Vec::with_capacity(lines.len());
    for li in 0..lines.len() {
        let line = lines.line(li);
        let b = line.as_bytes();
        let depth_start = depth;
        let in_literal = state != State::Code;
        let mut max_depth = depth;
        let mut i = 0;
        while i < b.len() {
            match state {
                State::BlockComment => {
                    if b[i..].starts_with(b"*/") {
                        state = State::Code;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                State::Double | State::Single | State::Backtick => {
                    let close = match state {
                        State::Double => b'"',
                        State::Single => b'\'',
                        _ => b'`',
                    };
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == close {
                        state = State::Code;
                    }
                    i += 1;
                }
                State::Triple => {
                    if b[i..].starts_with(b"\"\"\"") {
                        state = State::Code;
                        i += 3;
                    } else {
                        i += 1;
                    }
                }
                State::Raw(hashes) => {
                    if b[i] == b'"' && closes_raw(&b[i + 1..], hashes) {
                        state = State::Code;
                        i += 1 + hashes;
                    } else {
                        i += 1;
                    }
                }
                State::Code => {
                    if comments.iter().any(|c| b[i..].starts_with(c.as_bytes())) {
                        break;
                    }
                    if block && b[i..].starts_with(b"/*") {
                        state = State::BlockComment;
                        i += 2;
                        continue;
                    }
                    match b[i] {
                        b'"' => {
                            if lang.triple_quote_strings() && b[i..].starts_with(b"\"\"\"") {
                                state = State::Triple;
                                i += 3;
                                continue;
                            }
                            state = State::Double;
                        }
                        b'\'' => {
                            if lang.single_quote_strings() {
                                state = State::Single;
                            } else if let Some(len) = char_literal_len(&b[i..]) {
                                i += len;
                                continue;
                            }
                            // Otherwise a Rust lifetime or label: skip the quote.
                        }
                        b'`' if lang.backtick_strings() => state = State::Backtick,
                        b'r' if lang == Lang::Rust => {
                            if let Some((hashes, len)) = raw_string_open(b, i) {
                                state = State::Raw(hashes);
                                i += len;
                                continue;
                            }
                        }
                        b'{' => {
                            depth += 1;
                            max_depth = max_depth.max(depth);
                        }
                        b'}' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    i += 1;
                }
            }
        }
        // Char literals and most languages' quoted strings end with the
        // line; resetting bounds the damage of a lexing mistake. Rust string
        // literals may legitimately span lines, so they carry over.
        if state == State::Single || (state == State::Double && lang != Lang::Rust) {
            state = State::Code;
        }
        out.push(LineInfo {
            depth_start,
            depth_end: depth,
            max_depth: max_depth.max(depth_start),
            blank: line.trim().is_empty(),
            closer: false,
            in_literal,
        });
    }
    out
}

/// Length of a char literal starting at `b[0] == '\''`, or `None` for a
/// lifetime / label (`'a`, `'outer:`).
fn char_literal_len(b: &[u8]) -> Option<usize> {
    if b.len() >= 3 && b[1] != b'\\' && b[1] != b'\'' && b[1] < 0x80 && b[2] == b'\'' {
        return Some(3);
    }
    if b.len() >= 4 && (b[1] == b'\\' || b[1] >= 0x80) {
        let limit = b.len().min(12);
        return b[2..limit].iter().position(|&c| c == b'\'').map(|p| p + 3);
    }
    None
}

/// Detect `r"`, `r#"`, `br"`... at `b[i] == 'r'`. Returns the number of
/// hashes and the length of the opener.
fn raw_string_open(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let prev_ident = |j: usize| b[j].is_ascii_alphanumeric() || b[j] == b'_';
    if i > 0 {
        let before_ok = if b[i - 1] == b'b' {
            i == 1 || !prev_ident(i - 2)
        } else {
            !prev_ident(i - 1)
        };
        if !before_ok {
            return None;
        }
    }
    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
    (b.get(i + 1 + hashes) == Some(&b'"')).then_some((hashes, hashes + 2))
}

fn closes_raw(rest: &[u8], hashes: usize) -> bool {
    rest.len() >= hashes && rest[..hashes].iter().all(|&c| c == b'#')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depths(src: &str, lang: Lang) -> Vec<(u32, u32)> {
        let lines = Lines::new(src);
        brace_infos(&lines, lang)
            .iter()
            .map(|i| (i.depth_start, i.depth_end))
            .collect()
    }

    #[test]
    fn counts_structural_braces_only() {
        let src = "fn a() {\n    let s = \"{\";\n    // }\n    /* { */\n}\n";
        assert_eq!(
            depths(src, Lang::Rust),
            [(0, 1), (1, 1), (1, 1), (1, 1), (1, 0)]
        );
    }

    #[test]
    fn rust_lifetimes_chars_and_raw_strings() {
        let src =
            "fn f<'a>(x: &'a str) -> char {\n    let c = '{';\n    let r = r#\"}\"#;\n    '}'\n}";
        assert_eq!(
            depths(src, Lang::Rust),
            [(0, 1), (1, 1), (1, 1), (1, 1), (1, 0)]
        );
    }

    #[test]
    fn rust_strings_span_lines() {
        let src = "const S: &str = \"a {\nb\";\nfn x() {}";
        assert_eq!(depths(src, Lang::Rust), [(0, 0), (0, 0), (0, 0)]);
    }

    #[test]
    fn javascript_templates_span_lines() {
        let src = "const t = `\n{ not code }\n`;\nfunction f() {\n}";
        assert_eq!(
            depths(src, Lang::JavaScript),
            [(0, 0), (0, 0), (0, 0), (0, 1), (1, 0)]
        );
    }

    #[test]
    fn block_comments_span_lines() {
        let src = "/*\n{\n*/\nint main() {\n}";
        assert_eq!(
            depths(src, Lang::C),
            [(0, 0), (0, 0), (0, 0), (0, 1), (1, 0)]
        );
    }

    #[test]
    fn unbalanced_close_saturates() {
        assert_eq!(depths("}\n}", Lang::Go), [(0, 0), (0, 0)]);
    }
}
