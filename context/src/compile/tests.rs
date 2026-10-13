use super::*;

fn tf(p: &CompiledPassage, term: &str) -> u32 {
    p.terms
        .iter()
        .find(|(t, _)| t == term)
        .map_or(0, |(_, n)| *n)
}

#[test]
fn compiles_passages_with_title_boosted_terms() {
    let doc = SourceDoc::file(
        "repo",
        "docs/routing.md",
        "# Query routing\n\nThe router picks a path for each query.\n",
    )
    .unwrap();
    let c = compile_doc(doc, &CompileOptions::default()).unwrap();
    assert_eq!(c.meta.title, "routing.md");
    assert_eq!(c.meta.lang, Lang::Markdown);
    assert_eq!(c.passages.len(), 1);
    let p = &c.passages[0];
    // "router" appears once in the body; "rout" (routing) in heading + crumb.
    assert_eq!(tf(p, "router"), 1);
    assert!(
        tf(p, "rout") >= 4,
        "heading term counted in body and boosted via breadcrumb"
    );
    assert!(p.length >= 6);
    assert!(p.terms.windows(2).all(|w| w[0].0 < w[1].0), "terms sorted");
    assert_eq!(c.meta.source_hash, c.meta.text_hash);
}

#[test]
fn code_symbols_feed_title_terms_and_graph() {
    let doc = SourceDoc::file(
        "repo",
        "src/q.rs",
        "pub fn parse_query(s: &str) -> Query {\n    tokenize(s)\n}\n",
    )
    .unwrap();
    let c = compile_doc(doc, &CompileOptions::default()).unwrap();
    let p = &c.passages[0];
    assert!(tf(p, "parsequery") > 1, "symbol name boosted");
    assert!(p.facts.defines.iter().any(|e| e.key == "parse query"));
    assert!(p.facts.edges.iter().any(|e| e.object.key == "tokenize"));
}

#[test]
fn skips_binary_and_oversized_documents() {
    let opts = CompileOptions {
        max_doc_bytes: 10,
        ..CompileOptions::default()
    };
    let big = SourceDoc::note("big", "x".repeat(11)).unwrap();
    assert_eq!(compile_doc(big, &opts).unwrap_err().1, SkipReason::TooLarge);
    let bin = SourceDoc::note("bin", "a\0b").unwrap();
    assert_eq!(
        compile_doc(bin, &CompileOptions::default()).unwrap_err().1,
        SkipReason::Binary
    );
}

#[test]
fn compile_all_preserves_order_and_reports_skips() {
    let docs = vec![
        SourceDoc::note("a", "alpha text").unwrap(),
        SourceDoc::note("b", "bad\0").unwrap(),
        SourceDoc::note("c", "gamma text").unwrap(),
    ];
    let (ok, skipped) = compile_all(docs, &CompileOptions::default());
    let keys: Vec<&str> = ok.iter().map(|d| d.meta.key.as_str()).collect();
    assert_eq!(keys, ["note:a", "note:c"]);
    assert_eq!(skipped.len(), 1);
}

#[test]
fn empty_documents_compile_with_no_passages() {
    let c = compile_doc(
        SourceDoc::note("e", "").unwrap(),
        &CompileOptions::default(),
    )
    .unwrap();
    assert!(c.passages.is_empty());
}
