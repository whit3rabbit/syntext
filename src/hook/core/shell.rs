//! Minimal shell tokenization for hook command rewrites.
//!
//! This is intentionally not a shell interpreter. It only separates words and
//! top-level operators enough to avoid corrupting quoted arguments or rewriting
//! commands that depend on stdin, redirects, or runtime expansion.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Word {
    pub(crate) text: String,
    pub(crate) raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellItem {
    Command(Vec<Word>),
    Op(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellLine {
    pub(crate) items: Vec<ShellItem>,
    pub(crate) has_pipe: bool,
    pub(crate) has_redirection: bool,
    pub(crate) has_expansion: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellParseError {
    TrailingEscape,
    UnclosedQuote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    Single,
    Double,
}

pub(crate) fn parse(command: &str) -> Result<ShellLine, ShellParseError> {
    let chars: Vec<char> = command.chars().collect();
    let mut items = Vec::new();
    let mut words = Vec::new();
    let mut raw = String::new();
    let mut text = String::new();
    let mut state = State::Normal;
    let mut has_pipe = false;
    let mut has_redirection = false;
    let mut has_expansion = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match state {
            State::Normal => match c {
                c if c.is_whitespace() => {
                    finish_word(&mut words, &mut raw, &mut text);
                    i += 1;
                }
                '\'' => {
                    raw.push(c);
                    state = State::Single;
                    i += 1;
                }
                '"' => {
                    raw.push(c);
                    state = State::Double;
                    i += 1;
                }
                '\\' => {
                    raw.push(c);
                    i += 1;
                    if i >= chars.len() {
                        return Err(ShellParseError::TrailingEscape);
                    }
                    raw.push(chars[i]);
                    text.push(chars[i]);
                    i += 1;
                }
                '$' | '`' => {
                    has_expansion = true;
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
                '&' if chars.get(i + 1) == Some(&'&') => {
                    finish_word(&mut words, &mut raw, &mut text);
                    finish_command(&mut items, &mut words);
                    items.push(ShellItem::Op("&&".to_string()));
                    i += 2;
                }
                '|' if chars.get(i + 1) == Some(&'|') => {
                    finish_word(&mut words, &mut raw, &mut text);
                    finish_command(&mut items, &mut words);
                    items.push(ShellItem::Op("||".to_string()));
                    i += 2;
                }
                '|' => {
                    finish_word(&mut words, &mut raw, &mut text);
                    finish_command(&mut items, &mut words);
                    items.push(ShellItem::Op("|".to_string()));
                    has_pipe = true;
                    i += 1;
                }
                ';' => {
                    finish_word(&mut words, &mut raw, &mut text);
                    finish_command(&mut items, &mut words);
                    items.push(ShellItem::Op(";".to_string()));
                    i += 1;
                }
                '>' | '<' => {
                    finish_word(&mut words, &mut raw, &mut text);
                    finish_command(&mut items, &mut words);
                    let (op, next) = redirection_operator(&chars, i);
                    items.push(ShellItem::Op(op));
                    has_redirection = true;
                    i = next;
                }
                c if raw.is_empty() && c.is_ascii_digit() && is_fd_redirection(&chars, i) => {
                    finish_word(&mut words, &mut raw, &mut text);
                    finish_command(&mut items, &mut words);
                    let (op, next) = fd_redirection_operator(&chars, i);
                    items.push(ShellItem::Op(op));
                    has_redirection = true;
                    i = next;
                }
                _ => {
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
            },
            State::Single => {
                raw.push(c);
                if c == '\'' {
                    state = State::Normal;
                } else {
                    text.push(c);
                }
                i += 1;
            }
            State::Double => match c {
                '"' => {
                    raw.push(c);
                    state = State::Normal;
                    i += 1;
                }
                '\\' => {
                    raw.push(c);
                    i += 1;
                    if i >= chars.len() {
                        return Err(ShellParseError::TrailingEscape);
                    }
                    raw.push(chars[i]);
                    text.push(chars[i]);
                    i += 1;
                }
                '$' | '`' => {
                    has_expansion = true;
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
                _ => {
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
            },
        }
    }

    if state != State::Normal {
        return Err(ShellParseError::UnclosedQuote);
    }

    finish_word(&mut words, &mut raw, &mut text);
    finish_command(&mut items, &mut words);

    Ok(ShellLine {
        items,
        has_pipe,
        has_redirection,
        has_expansion,
    })
}

fn finish_word(words: &mut Vec<Word>, raw: &mut String, text: &mut String) {
    if raw.is_empty() {
        return;
    }
    words.push(Word {
        text: std::mem::take(text),
        raw: std::mem::take(raw),
    });
}

fn finish_command(items: &mut Vec<ShellItem>, words: &mut Vec<Word>) {
    if words.is_empty() {
        return;
    }
    items.push(ShellItem::Command(std::mem::take(words)));
}

fn redirection_operator(chars: &[char], start: usize) -> (String, usize) {
    let mut end = start + 1;
    if chars.get(end) == Some(&chars[start]) {
        end += 1;
    }
    if chars.get(end) == Some(&'&') {
        end += 1;
        if matches!(chars.get(end), Some(c) if c.is_ascii_digit()) {
            end += 1;
        }
    }
    (chars[start..end].iter().collect(), end)
}

fn is_fd_redirection(chars: &[char], start: usize) -> bool {
    let mut i = start;
    while matches!(chars.get(i), Some(c) if c.is_ascii_digit()) {
        i += 1;
    }
    matches!(chars.get(i), Some('>' | '<'))
}

fn fd_redirection_operator(chars: &[char], start: usize) -> (String, usize) {
    let mut i = start;
    while matches!(chars.get(i), Some(c) if c.is_ascii_digit()) {
        i += 1;
    }
    let (rest, end) = redirection_operator(chars, i);
    let mut op: String = chars[start..i].iter().collect();
    op.push_str(&rest);
    (op, end)
}

pub(crate) fn is_env_assignment(word: &str) -> bool {
    let Some(eq) = word.find('=') else {
        return false;
    };
    let name = &word[..eq];
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

pub(crate) fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '.' | '/' | ':' | '@' | '%' | '+' | '=' | ',')
    }) {
        return value.to_string();
    }

    let mut quoted = String::from("'");
    for c in value.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

pub(crate) fn render_raw_words(words: &[Word]) -> String {
    words
        .iter()
        .map(|word| word.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_words(command: &str) -> Vec<String> {
        let parsed = parse(command).unwrap();
        match &parsed.items[0] {
            ShellItem::Command(words) => words.iter().map(|w| w.text.clone()).collect(),
            ShellItem::Op(_) => panic!("expected command"),
        }
    }

    #[test]
    fn shell_parse_preserves_quoted_words() {
        assert_eq!(
            command_words(r#"rg "parse query" 'src/lib.rs'"#),
            vec!["rg", "parse query", "src/lib.rs"]
        );
    }

    #[test]
    fn shell_parse_detects_control_operators() {
        let parsed = parse("rg foo src && rg bar tests; echo done").unwrap();
        assert_eq!(
            parsed.items,
            vec![
                ShellItem::Command(vec![
                    Word {
                        text: "rg".to_string(),
                        raw: "rg".to_string()
                    },
                    Word {
                        text: "foo".to_string(),
                        raw: "foo".to_string()
                    },
                    Word {
                        text: "src".to_string(),
                        raw: "src".to_string()
                    },
                ]),
                ShellItem::Op("&&".to_string()),
                ShellItem::Command(vec![
                    Word {
                        text: "rg".to_string(),
                        raw: "rg".to_string()
                    },
                    Word {
                        text: "bar".to_string(),
                        raw: "bar".to_string()
                    },
                    Word {
                        text: "tests".to_string(),
                        raw: "tests".to_string()
                    },
                ]),
                ShellItem::Op(";".to_string()),
                ShellItem::Command(vec![
                    Word {
                        text: "echo".to_string(),
                        raw: "echo".to_string()
                    },
                    Word {
                        text: "done".to_string(),
                        raw: "done".to_string()
                    },
                ]),
            ]
        );
    }

    #[test]
    fn shell_parse_flags_pipes_and_redirects() {
        assert!(parse("cat file | grep foo").unwrap().has_pipe);
        assert!(parse("rg foo > out.txt").unwrap().has_redirection);
        assert!(parse("rg foo 2>&1").unwrap().has_redirection);
    }

    #[test]
    fn shell_parse_flags_runtime_expansion() {
        assert!(parse(r#"rg "$PATTERN" src"#).unwrap().has_expansion);
        assert!(!parse(r#"rg '$PATTERN' src"#).unwrap().has_expansion);
    }

    #[test]
    fn shell_parse_rejects_unbalanced_quotes() {
        assert_eq!(
            parse(r#"rg "unterminated"#).unwrap_err(),
            ShellParseError::UnclosedQuote
        );
    }

    #[test]
    fn shell_quote_handles_spaces_and_quotes() {
        assert_eq!(shell_quote("src/lib.rs"), "src/lib.rs");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("don't"), "'don'\\''t'");
    }
}
