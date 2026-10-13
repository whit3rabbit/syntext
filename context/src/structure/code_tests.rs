use super::*;
use crate::structure::{segment, Layout};
use crate::types::DocKind;

fn seg(text: &str, lang: Lang, max: usize) -> Layout {
    segment(
        text,
        lang,
        DocKind::File,
        "f",
        &SegmentOptions {
            max_passage_bytes: max,
            min_passage_bytes: 0,
        },
    )
}

fn bodies<'a>(text: &'a str, layout: &Layout) -> Vec<&'a str> {
    layout
        .passages
        .iter()
        .map(|p| text[p.start..p.end].trim_end())
        .collect()
}

#[test]
fn blank_lines_inside_bodies_do_not_split_units() {
    let text = "fn a() {\n    one();\n\n    two();\n}\n\nfn b() {}\n";
    let layout = seg(text, Lang::Rust, 1800);
    assert_eq!(
        bodies(text, &layout),
        ["fn a() {\n    one();\n\n    two();\n}", "fn b() {}"]
    );
}

#[test]
fn closed_block_starts_a_new_unit_without_blank_line() {
    let text = "fn a() {\n    x();\n}\n#[test]\nfn b() {\n    y();\n}\n";
    let layout = seg(text, Lang::Rust, 1800);
    assert_eq!(layout.passages.len(), 2);
    assert!(bodies(text, &layout)[1].starts_with("#[test]"));
    assert_eq!(layout.passages[1].symbol.as_ref().unwrap().name, "b");
    assert_eq!(
        layout.passages[1].anchor_offset, 1,
        "attribute precedes def line"
    );
}

#[test]
fn else_and_catch_continue_the_unit() {
    let text = "if (x) {\n  a();\n}\nelse {\n  b();\n}\n";
    let layout = seg(text, Lang::JavaScript, 1800);
    assert_eq!(layout.passages.len(), 1);
}

#[test]
fn indentation_languages_end_units_on_dedent() {
    let text = "def a():\n    return 1\ndef b():\n    return 2\n";
    let layout = seg(text, Lang::Python, 1800);
    let names: Vec<&str> = layout
        .passages
        .iter()
        .map(|p| p.symbol.as_ref().unwrap().name.as_str())
        .collect();
    assert_eq!(names, ["a", "b"]);
}

#[test]
fn ruby_end_closes_a_unit() {
    let text = "def a\n  1\nend\ndef b\n  2\nend\n";
    let layout = seg(text, Lang::Ruby, 1800);
    assert_eq!(layout.passages.len(), 2);
    assert!(bodies(text, &layout)[0].ends_with("end"));
}

#[test]
fn signature_anchor_spans_multi_line_signatures() {
    let text = "pub fn search(\n    &self,\n    pattern: &str,\n) -> usize {\n    0\n}\n";
    let layout = seg(text, Lang::Rust, 1800);
    let p = &layout.passages[0];
    assert_eq!((p.anchor_offset, p.anchor_len), (0, 4));
}

#[test]
fn oversized_python_class_splits_into_methods() {
    let mut text = String::from("class Service:\n    \"\"\"Doc.\"\"\"\n\n");
    for name in ["start", "stop", "status"] {
        text.push_str(&format!(
            "    def {name}(self):\n        value = compute_{name}()\n        return value\n\n"
        ));
    }
    let layout = seg(&text, Lang::Python, 120);
    let methods: Vec<(&str, SymbolKind, &str)> = layout
        .passages
        .iter()
        .filter_map(|p| {
            p.symbol
                .as_ref()
                .map(|s| (s.name.as_str(), s.kind, p.breadcrumb.as_str()))
        })
        .collect();
    assert!(methods.contains(&("stop", SymbolKind::Method, "f > class Service")));
    assert!(methods.contains(&("status", SymbolKind::Method, "f > class Service")));
}

#[test]
fn toml_tables_start_units() {
    let text = "[package]\nname = \"x\"\n[dependencies]\nserde = \"1\"\n";
    let layout = seg(text, Lang::Toml, 1800);
    assert_eq!(layout.passages.len(), 2);
}

#[test]
fn a_split_function_is_defined_by_its_first_piece() {
    let mut text =
        String::from("/// Route a query.\npub fn route_query(pattern: &str) -> Route {\n");
    for i in 0..6 {
        text.push_str(&format!(
            "    if pattern.len() > {i} {{\n        let step_{i} = prepare({i});\n        record(step_{i});\n    }}\n\n"
        ));
    }
    text.push_str("    Route::Scan\n}\n");
    let layout = seg(&text, Lang::Rust, 160);
    assert!(layout.passages.len() > 2, "the function is split");
    let first = &layout.passages[0];
    assert_eq!(first.kind, PassageKind::Definition);
    assert_eq!(
        first.symbol.as_ref().map(|s| s.name.as_str()),
        Some("route_query")
    );
    let anchor = first.line_start as usize + first.anchor_offset as usize;
    assert!(text
        .lines()
        .nth(anchor - 1)
        .unwrap()
        .starts_with("pub fn route_query"));
    for p in &layout.passages[1..] {
        assert_eq!(p.kind, PassageKind::Code);
        assert_eq!(p.breadcrumb, "f > function route_query");
    }
}
