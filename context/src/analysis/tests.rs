use super::*;

fn texts(input: &str) -> Vec<String> {
    analyze(input).into_iter().map(|t| t.text).collect()
}

#[test]
fn identifiers_yield_compound_and_stemmed_subwords() {
    let terms = analyze("fn parse_query(args)");
    let got: Vec<(&str, u32, bool)> = terms
        .iter()
        .map(|t| (t.text.as_str(), t.pos, t.compound))
        .collect();
    assert_eq!(
        got,
        [
            ("fn", 0, false),
            ("parsequery", 1, true),
            ("pars", 1, false),
            ("queri", 2, false),
            ("arg", 3, false),
        ]
    );
}

#[test]
fn casing_conventions_share_one_compound() {
    for ident in ["parse_query", "parseQuery", "ParseQuery", "PARSE_QUERY"] {
        assert!(
            texts(ident).contains(&"parsequery".to_string()),
            "{ident} -> {:?}",
            texts(ident)
        );
    }
}

#[test]
fn term_spans_point_at_source_bytes() {
    let text = "call parseQuery now";
    for term in analyze(text) {
        assert!(term.end <= text.len());
        assert!(term.start < term.end);
    }
    let sub = analyze(text)
        .into_iter()
        .find(|t| t.text == "queri")
        .unwrap();
    assert_eq!(&text[sub.start..sub.end], "Query");
}

#[test]
fn stopwords_drop_but_leave_position_gaps() {
    let terms = analyze("query of the router");
    let got: Vec<(&str, u32)> = terms.iter().map(|t| (t.text.as_str(), t.pos)).collect();
    assert_eq!(got, [("queri", 0), ("router", 3)]);
}

#[test]
fn noise_tokens_are_skipped() {
    assert!(texts("a b c x").is_empty(), "single letters dropped");
    assert!(texts(&"f".repeat(80)).is_empty(), "overlong word dropped");
    assert!(
        texts("3f2a9c4d5e6f7a8b9c0d1e2f3a4b").is_empty(),
        "hash dropped"
    );
    assert!(texts("1234567890123456").is_empty(), "long number dropped");
    assert_eq!(texts("v2 404 7"), ["v2", "404", "7"]);
}

#[test]
fn unicode_lowercases_without_stemming() {
    assert_eq!(texts("Größe naïve"), ["größe", "naïve"]);
    assert_eq!(texts("検索"), ["検", "索"]);
}

#[test]
fn fold_key_normalizes_entity_names() {
    assert_eq!(fold_key("parse_query"), "parse query");
    assert_eq!(fold_key("parseQuery"), "parse query");
    assert_eq!(fold_key("Parse Query"), "parse query");
    assert_eq!(fold_key("Index::search"), "index search");
    assert_eq!(fold_key("Alice's"), "alice");
    assert_eq!(fold_key("Alice\u{2019}s team"), "alice team");
    assert_eq!(fold_key("HTTPServer"), "http server");
    assert_eq!(fold_key("Meridian Labs"), "meridian labs");
    assert_eq!(fold_key("::"), "");
}

#[test]
fn entity_terms_match_what_analysis_emits() {
    for ident in [
        "parse_query",
        "parseQuery",
        "HTTPServer",
        "search",
        "Index",
        "get_x",
        "MemIndex",
    ] {
        let term = entity_term(&fold_key(ident)).unwrap();
        assert!(
            texts(ident).contains(&term),
            "{ident}: entity term {term:?} not in {:?}",
            texts(ident)
        );
    }
    assert_eq!(entity_term(""), None);
    assert_eq!(entity_term("the"), None, "stopword-only key");
}

#[test]
fn term_set_is_sorted_and_deduplicated() {
    assert_eq!(term_set("works for works FOR"), ["work"]);
    assert_eq!(term_set("founded by"), ["found"]);
}
