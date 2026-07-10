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
fn shell_parse_flags_background_operator() {
    // Bare `&` must force pass-through; `&&` is a list operator, not
    // backgrounding.
    assert!(parse("rg foo &").unwrap().has_background);
    assert!(!parse("rg foo && bar").unwrap().has_background);
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
