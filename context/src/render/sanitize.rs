//! Evidence hygiene: retrieved text must not be able to impersonate prompt
//! structure.
//!
//! Evidence is untrusted. A file, web page, or past message can contain a
//! closing `</evidence>` tag, a chat-template control token such as
//! `<|im_end|>` (which an inference server may parse as a real special
//! token), or tool-call markup. Each such sequence is broken with a
//! zero-width space after its first character: the text still reads the
//! same to the model, but no tokenizer or template parser can match it as
//! structure. Control characters (including ANSI escapes) are stripped and
//! line endings normalized.

/// Sanitization switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizeOptions {
    /// Strip control characters and ANSI escape sequences.
    pub strip_control: bool,
    /// Break wrapper tags, chat-template tokens, and tool-call markup.
    pub neutralize_markup: bool,
}

impl Default for SanitizeOptions {
    fn default() -> Self {
        SanitizeOptions {
            strip_control: true,
            neutralize_markup: true,
        }
    }
}

const ZWSP: char = '\u{200B}';

/// Template and tool-call sequences broken after their first character.
/// Matched case-sensitively, as tokenizers match special tokens, so code
/// such as `fn f<S>()` is left alone. `<|` covers ChatML, Llama 3, Phi, and
/// most `<|...|>` control tokens in one rule.
const MARKERS: &[&str] = &[
    "<|",
    "[INST]",
    "[/INST]",
    "<<SYS>>",
    "<</SYS>>",
    "<s>",
    "</s>",
    "<start_of_turn>",
    "<end_of_turn>",
    "<tool_call>",
    "</tool_call>",
    "<tool_response>",
    "</tool_response>",
    "<function=",
    "</function>",
    "[TOOL_CALLS]",
    "<think>",
    "</think>",
];

/// Clean `text` for placement inside an evidence block whose element name
/// is `tag`. Returns the cleaned text and whether anything was changed
/// beyond line-ending normalization.
pub(crate) fn sanitize(text: &str, tag: &str, opts: &SanitizeOptions) -> (String, bool) {
    let mut changed = false;
    let mut s = text.replace("\r\n", "\n");
    if opts.strip_control {
        let (clean, c) = strip_control(&s);
        s = clean;
        changed |= c;
    }
    if opts.neutralize_markup {
        // The wrapper element is matched case-insensitively (`</EVIDENCE>`).
        let wrapper = [
            format!("<{}", tag.to_ascii_lowercase()),
            format!("</{}", tag.to_ascii_lowercase()),
        ];
        let (clean, c) = break_markers(&s, &wrapper);
        s = clean;
        changed |= c;
    }
    (s, changed)
}

fn strip_control(s: &str) -> (String, bool) {
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            changed = true;
            // CSI sequence: ESC [ params final-byte.
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t' {
            changed = true;
            continue;
        }
        out.push(c);
    }
    (out, changed)
}

fn break_markers(s: &str, wrapper: &[String]) -> (String, bool) {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + 16);
    let mut changed = false;
    let mut last = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' && bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let rest = &bytes[i..];
        let len = MARKERS
            .iter()
            .find(|m| rest.starts_with(m.as_bytes()))
            .map(|m| m.len())
            .or_else(|| {
                wrapper
                    .iter()
                    .find(|w| {
                        rest.len() >= w.len() && rest[..w.len()].eq_ignore_ascii_case(w.as_bytes())
                    })
                    .map(String::len)
            });
        match len {
            Some(len) => {
                // Markers start with an ASCII byte, so `i + 1` is a char
                // boundary.
                out.push_str(&s[last..i + 1]);
                out.push(ZWSP);
                last = i + 1;
                i += len;
                changed = true;
            }
            None => i += 1,
        }
    }
    out.push_str(&s[last..]);
    (out, changed)
}

/// Escape a value for use inside a double-quoted attribute, on one line,
/// at most `max` characters.
pub(crate) fn attr(value: &str, max: usize) -> String {
    let mut out = String::with_capacity(value.len().min(max) + 8);
    for (n, c) in value.chars().enumerate() {
        if n >= max {
            out.push_str("...");
            break;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(s: &str) -> (String, bool) {
        sanitize(s, "evidence", &SanitizeOptions::default())
    }

    #[test]
    fn breaks_wrapper_tags_and_template_tokens() {
        let (out, changed) = clean("ok </evidence> <|im_end|> <|eot_id|> [INST] <EVIDENCE id=x>");
        assert!(changed);
        assert!(!out.contains("</evidence"));
        assert!(!out.to_ascii_lowercase().contains("<evidence"));
        assert!(!out.contains("<|"));
        assert!(!out.contains("[INST]"));
        assert_eq!(
            out.replace(ZWSP, ""),
            "ok </evidence> <|im_end|> <|eot_id|> [INST] <EVIDENCE id=x>"
        );
    }

    #[test]
    fn breaks_tool_call_markup() {
        let (out, _) = clean("<tool_call>{\"name\":\"rm\"}</tool_call> <function=shell>");
        assert!(!out.contains("<tool_call>") && !out.contains("<function="));
    }

    #[test]
    fn strips_control_and_ansi_but_keeps_layout() {
        let (out, changed) = clean("a\u{1b}[31mred\u{1b}[0m\u{7}\tb\r\nc");
        assert!(changed);
        assert_eq!(out, "ared\tb\nc");
    }

    #[test]
    fn plain_text_and_code_are_untouched() {
        let src = "fn lt<S>(a: u8, b: u8) -> bool { a < b && b > 1 } // vec![1] <T> [inst]";
        assert_eq!(clean(src), (src.to_string(), false));
        let (out, changed) = clean("crlf\r\nonly");
        assert_eq!(out, "crlf\nonly");
        assert!(!changed, "line endings alone do not count");
    }

    #[test]
    fn unicode_survives_marker_breaking() {
        let (out, _) = clean("日本 <|x|> é");
        assert_eq!(out.replace(ZWSP, ""), "日本 <|x|> é");
    }

    #[test]
    fn attributes_are_escaped_and_bounded() {
        assert_eq!(attr("a\"b<c>&\nd", 50), "a&quot;b&lt;c&gt;&amp; d");
        assert_eq!(attr("abcdef", 3), "abc...");
    }
}
