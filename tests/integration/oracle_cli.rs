#[path = "oracle_helpers.rs"]
mod oracle_helpers;

use oracle_helpers::{
    generate_corpus, generate_flags, generate_query, run_differential, run_differential_with_tier_c,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Golden Smoke Suite (Phase 2.5)
// ---------------------------------------------------------------------------

#[test]
fn golden_smoke_literal_basic() {
    let corpus = vec![
        ("src/main.rs".to_string(), b"fn parse_query() {}\n".to_vec()),
        ("src/lib.rs".to_string(), b"fn reparse() {}\n".to_vec()),
    ];
    run_differential(&corpus, "parse_query", &[]).unwrap();
}

#[test]
fn golden_smoke_anchored_literal() {
    let corpus = vec![
        (
            "src/main.rs".to_string(),
            b"fn _parse_query_() {}\n".to_vec(),
        ),
        (
            "src/lib.rs".to_string(),
            b"fn reparse_query() {}\n".to_vec(),
        ),
    ];
    run_differential(&corpus, "_parse_query_", &[]).unwrap();
}

#[test]
fn golden_smoke_crlf_line_endings() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"fn foo() {}\r\nfn bar() {}\r\n".to_vec(),
    )];
    run_differential(&corpus, "foo", &[]).unwrap();
    run_differential(&corpus, "bar", &[]).unwrap();
}

#[test]
fn golden_smoke_cr_only_line_endings() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"fn foo() {}\rfn bar() {}\r".to_vec(),
    )];
    run_differential(&corpus, "foo", &[]).unwrap();
}

#[test]
fn golden_smoke_utf8_bom() {
    let mut content = vec![0xEF, 0xBB, 0xBF];
    content.extend_from_slice(b"fn parse_query() {}\n");
    let corpus = vec![("src/main.rs".to_string(), content)];
    run_differential(&corpus, "parse_query", &[]).unwrap();
}

#[test]
fn golden_smoke_utf16_le() {
    let content_u16 = [
        0xFF, 0xFE, // BOM
        b'f', 0, b'n', 0, b' ', 0, b'p', 0, b'a', 0, b'r', 0, b's', 0, b'e', 0, b'\n', 0,
    ];
    let corpus = vec![("src/main.rs".to_string(), content_u16.to_vec())];
    run_differential(&corpus, "parse", &[]).unwrap();
}

#[test]
fn golden_smoke_utf16_be() {
    let content_u16 = [
        0xFE, 0xFF, // BOM
        0, b'f', 0, b'n', 0, b' ', 0, b'p', 0, b'a', 0, b'r', 0, b's', 0, b'e', 0, b'\n',
    ];
    let corpus = vec![("src/main.rs".to_string(), content_u16.to_vec())];
    run_differential(&corpus, "parse", &[]).unwrap();
}

#[test]
fn golden_smoke_binary_null_at_8191() {
    let mut content = vec![b'a'; 8191];
    content.push(0); // binary
    content.extend_from_slice(b"fn parse_query() {}\n");
    let corpus = vec![("src/main.rs".to_string(), content)];
    run_differential(&corpus, "parse_query", &[]).unwrap();
}

#[test]
fn golden_smoke_long_token() {
    let mut content = b"fn ".to_vec();
    content.extend_from_slice(&[b'a'; 130]);
    content.extend_from_slice(b"() {}\n");
    let corpus = vec![("src/main.rs".to_string(), content)];
    let query = String::from_utf8(vec![b'a'; 130]).unwrap();
    run_differential(&corpus, &query, &[]).unwrap();
}

#[test]
fn golden_smoke_empty_file() {
    let corpus = vec![("src/main.rs".to_string(), Vec::new())];
    run_differential(&corpus, "parse", &[]).unwrap();
}

#[test]
fn golden_smoke_invalid_utf8_in_content() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        vec![
            b'f', b'n', b' ', b'p', b'a', b'r', b's', b'e', 0xFF, 0xFE, b'\n',
        ],
    )];
    run_differential(&corpus, "parse", &[]).unwrap();
}

#[test]
fn golden_smoke_case_insensitive() {
    let corpus = vec![("src/main.rs".to_string(), b"fn PARSE_QUERY() {}\n".to_vec())];
    run_differential(&corpus, "parse_query", &["-i"]).unwrap();
}

#[test]
fn golden_smoke_alternation() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"fn parse() {}\nfn query() {}\n".to_vec(),
    )];
    run_differential(&corpus, "parse|query", &[]).unwrap();
}

#[test]
fn golden_smoke_char_class_mid_token() {
    let corpus = vec![("src/main.rs".to_string(), b"fn parse_query() {}\n".to_vec())];
    run_differential(&corpus, "parse_quer[yi]", &[]).unwrap();
}

// ---------------------------------------------------------------------------
// Phase 4.2: Targeted Flag-Interaction Golden Smoke Tests
// ---------------------------------------------------------------------------

/// Fixed-string multi-pattern search: test each pattern independently.
/// Note: st does not support the `-e` flag for multi-pattern OR in a single
/// invocation (that is a rg-specific flag). Each pattern is tested separately.
#[test]
fn golden_smoke_fixed_multi_pattern() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"fn parse_query() {}\nfn reparse() {}\nfn query_all() {}\n".to_vec(),
    )];
    // Test each fixed literal independently — both engines agree on single-pattern results.
    run_differential(&corpus, "parse_query", &["-F"]).unwrap();
    run_differential(&corpus, "query_all", &["-F"]).unwrap();
}

/// -w with an alternation pattern.
#[test]
fn golden_smoke_word_regexp_alternation() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"foo bar foobar barfoo\n".to_vec(),
    )];
    run_differential(&corpus, "foo|bar", &["-w"]).unwrap();
}

/// -o only-matching (no context lines).
#[test]
fn golden_smoke_only_matching_context() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"before\nfn parse_query() {}\nafter\n".to_vec(),
    )];
    run_differential(&corpus, "parse", &["-o"]).unwrap();
}

/// -c (count matching lines) — Tier C rendered-output comparison.
#[test]
fn golden_smoke_count_lines() {
    let corpus = vec![
        (
            "src/main.rs".to_string(),
            b"fn parse() {}\nfn parse_query() {}\nfn other() {}\n".to_vec(),
        ),
        ("src/lib.rs".to_string(), b"fn parse_all() {}\n".to_vec()),
    ];
    run_differential_with_tier_c(&corpus, "parse", &["-c"]).unwrap();
}

/// -l (files-with-matches) — Tier C rendered-output comparison.
#[test]
fn golden_smoke_files_with_matches() {
    let corpus = vec![
        ("src/main.rs".to_string(), b"fn parse_query() {}\n".to_vec()),
        ("src/lib.rs".to_string(), b"fn reparse() {}\n".to_vec()),
        ("src/util.rs".to_string(), b"fn helper() {}\n".to_vec()),
    ];
    run_differential_with_tier_c(&corpus, "parse", &["-l"]).unwrap();
}

/// --vimgrep — Tier C rendered-output comparison (column field normalized away).
#[test]
fn golden_smoke_vimgrep() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"fn parse_query() {}\nfn reparse() {}\n".to_vec(),
    )];
    run_differential_with_tier_c(&corpus, "parse", &["--vimgrep"]).unwrap();
}

/// -x (line-regexp).
#[test]
fn golden_smoke_line_regexp() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"parse\nfn parse_query() {}\nparse_all\n".to_vec(),
    )];
    run_differential(&corpus, "parse", &["-x"]).unwrap();
}

/// -w (word-regexp).
#[test]
fn golden_smoke_word_regexp() {
    let corpus = vec![(
        "src/main.rs".to_string(),
        b"parse parse_query reparse\n".to_vec(),
    )];
    run_differential(&corpus, "parse", &["-w"]).unwrap();
}

// ---------------------------------------------------------------------------
// Property Tests via CLI Runner (Phase 3.3 + Phase 4.2)
// ---------------------------------------------------------------------------

fn generate_cli_run() -> impl Strategy<Value = (Vec<(String, Vec<u8>)>, String, Vec<&'static str>)>
{
    generate_corpus().prop_flat_map(|corpus| {
        let query_strat = generate_query(&corpus);
        let flags_strat = generate_flags();
        (Just(corpus), query_strat, flags_strat)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]
    #[test]
    fn test_cli_differential((corpus, query, flags) in generate_cli_run()) {
        // Filter out -F if query contains regex metacharacters (caller responsibility per DIVERGENCES.md)
        let has_regex_meta = query.chars().any(|c| matches!(c, '.' | '*' | '+' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '|' | '^' | '$' | '\\'));
        let effective_flags: Vec<&str> = flags.iter()
            .filter(|&&f| !(f == "-F" && has_regex_meta))
            .copied()
            .collect();

        let res = run_differential_with_tier_c(&corpus, &query, &effective_flags);
        assert!(res.is_ok(), "differential test failed: {:?}", res);
    }
}

#[test]
fn test_regression_fixtures() {
    use serde::Deserialize;
    use std::fs;

    #[derive(Deserialize)]
    struct RegressionFixture {
        r#type: String,
        corpus: Vec<FileEntry>,
        query: String,
        flags: Vec<String>,
    }

    #[derive(Deserialize)]
    struct FileEntry {
        path: String,
        content_b64: String,
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let regressions_dir = std::path::Path::new(manifest_dir)
        .join("tests")
        .join("oracle")
        .join("regressions");

    if !regressions_dir.exists() {
        return;
    }

    let entries = fs::read_dir(regressions_dir).unwrap();
    let mut count = 0;
    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            count += 1;
            println!("Running regression fixture: {:?}", path);
            let content = fs::read_to_string(&path).unwrap();
            let fixture: RegressionFixture = serde_json::from_str(&content).unwrap();

            let corpus: Vec<(String, Vec<u8>)> = fixture
                .corpus
                .iter()
                .map(|fe| {
                    let bytes = oracle_helpers::base64_decode(&fe.content_b64)
                        .unwrap_or_else(|e| panic!("invalid base64 in fixture {:?}: {}", path, e));
                    (fe.path.clone(), bytes)
                })
                .collect();

            let flags_refs: Vec<&str> = fixture.flags.iter().map(|s| s.as_str()).collect();

            if fixture.r#type == "cli" {
                if let Err(e) = run_differential_with_tier_c(&corpus, &fixture.query, &flags_refs) {
                    panic!("Regression fixture failed: {:?}\nError: {}", path, e);
                }
            } else {
                panic!("Unknown regression fixture type: {}", fixture.r#type);
            }
        }
    }
    println!("Successfully ran {} regression fixtures", count);
}
