use super::*;
use proptest::prelude::*;

fn opts(max: usize, min: usize) -> SegmentOptions {
    SegmentOptions {
        max_passage_bytes: max,
        min_passage_bytes: min,
    }
}

fn texts<'a>(text: &'a str, layout: &Layout) -> Vec<&'a str> {
    layout
        .passages
        .iter()
        .map(|p| &text[p.start..p.end])
        .collect()
}

/// Invariants every layout must satisfy, whatever the input.
fn check_invariants(text: &str, layout: &Layout, max: usize) {
    let lines = Lines::new(text);
    let mut prev_end = 0;
    for p in &layout.passages {
        assert!(p.start < p.end, "empty passage {p:?}");
        assert!(p.end <= text.len());
        assert!(p.start >= prev_end, "overlap or disorder at {p:?}");
        assert!(text.is_char_boundary(p.start) && text.is_char_boundary(p.end));
        assert!(p.line_start >= 1 && p.line_start <= p.line_end);
        assert_eq!(p.line_start as usize, lines.line_of(p.start) + 1);
        assert!(p.line_end as usize <= lines.len());
        assert!(
            p.end - p.start <= max || !text[p.start..p.end].contains('\n'),
            "oversized multi-line passage: {} > {max}",
            p.end - p.start
        );
        if p.anchor_len > 0 {
            assert!(p.line_start + p.anchor_offset <= p.line_end);
        }
        prev_end = p.end;
    }
}

#[test]
fn markdown_sections_drive_breadcrumbs() {
    let text = "# Guide\n\nIntro paragraph.\n\n## Install\n\nRun the installer.\n\n### macOS\n\nUse brew.\n\n## Usage\n\nCall it.\n";
    let layout = segment(
        text,
        Lang::Markdown,
        DocKind::File,
        "docs/guide.md",
        &opts(1800, 0),
    );
    let crumbs: Vec<&str> = layout
        .passages
        .iter()
        .map(|p| p.breadcrumb.as_str())
        .collect();
    assert_eq!(
        crumbs,
        [
            "docs/guide.md > Guide",
            "docs/guide.md > Guide > Install",
            "docs/guide.md > Guide > Install > macOS",
            "docs/guide.md > Guide > Usage",
        ]
    );
    let first = &layout.passages[0];
    assert!(text[first.start..first.end].starts_with("# Guide"));
    assert_eq!(first.anchor_len, 1, "heading is the anchor");
    assert_eq!(layout.sections.len(), 4);
    assert_eq!(layout.sections[2].parent, Some(1));
    assert_eq!(layout.sections[1].end, layout.sections[3].start);
    check_invariants(text, &layout, 1800);
}

#[test]
fn rust_impl_splits_into_method_passages() {
    let method = |name: &str| {
        format!(
            "    /// Doc for {name}.\n    pub fn {name}(&self) -> usize {{\n        let total = self.items.len();\n        total + {}\n    }}\n",
            name.len()
        )
    };
    let mut text = String::from("use std::fmt;\n\n/// The index.\npub struct Index {\n    items: Vec<u8>,\n}\n\nimpl Index {\n");
    for name in ["alpha", "beta", "gamma", "delta"] {
        text.push_str(&method(name));
        text.push('\n');
    }
    text.push_str("}\n");
    let layout = segment(
        &text,
        Lang::Rust,
        DocKind::File,
        "src/index.rs",
        &opts(240, 0),
    );
    check_invariants(&text, &layout, 240);
    let syms: Vec<(String, SymbolKind, String)> = layout
        .passages
        .iter()
        .filter_map(|p| {
            p.symbol
                .as_ref()
                .map(|s| (s.name.clone(), s.kind, p.breadcrumb.clone()))
        })
        .collect();
    assert!(syms.contains(&("Index".into(), SymbolKind::Struct, "src/index.rs".into())));
    for name in ["beta", "gamma", "delta"] {
        assert!(
            syms.contains(&(
                name.into(),
                SymbolKind::Method,
                "src/index.rs > impl Index".into()
            )),
            "{name} missing from {syms:?}"
        );
    }
    let delta = layout
        .passages
        .iter()
        .find(|p| p.symbol.as_ref().is_some_and(|s| s.name == "delta"))
        .unwrap();
    let body = &text[delta.start..delta.end];
    assert!(body.contains("/// Doc for delta."), "doc comment attached");
    assert!(
        body.trim_end().ends_with('}'),
        "closing brace of impl attached to last method"
    );
    assert_eq!(delta.kind, PassageKind::Definition);
}

#[test]
fn python_classes_and_functions() {
    let text = "import os\n\n\nclass Router:\n    \"\"\"Routes requests.\"\"\"\n\n    def route(self, path):\n        return path\n\n\ndef main():\n    Router().route('/')\n";
    let layout = segment(text, Lang::Python, DocKind::File, "app.py", &opts(1800, 0));
    check_invariants(text, &layout, 1800);
    let names: Vec<&str> = layout
        .passages
        .iter()
        .filter_map(|p| p.symbol.as_ref().map(|s| s.name.as_str()))
        .collect();
    assert_eq!(names, ["Router", "main"]);
}

#[test]
fn small_units_merge_within_budget() {
    let text = "use a;\nuse b;\n\nconst X: u8 = 1;\n\nfn tiny() {}\n";
    let layout = segment(text, Lang::Rust, DocKind::File, "x.rs", &opts(1800, 240));
    assert_eq!(layout.passages.len(), 1, "{:?}", texts(text, &layout));
    let p = &layout.passages[0];
    assert_eq!(
        p.symbol.as_ref().unwrap().name,
        "tiny",
        "function outranks constant"
    );
    assert_eq!(p.kind, PassageKind::Definition);
}

#[test]
fn turns_and_data_kinds() {
    let layout = segment(
        "hello there\n\nsecond",
        Lang::Text,
        DocKind::Turn,
        "turn 1",
        &opts(1800, 0),
    );
    assert!(layout.passages.iter().all(|p| p.kind == PassageKind::Turn));
    let json = "{\n  \"name\": \"x\",\n  \"deps\": {\n    \"a\": 1\n  }\n}\n";
    let layout = segment(json, Lang::Json, DocKind::File, "p.json", &opts(1800, 0));
    assert!(layout.passages.iter().all(|p| p.kind == PassageKind::Data));
    assert!(layout.passages.iter().all(|p| p.symbol.is_none()));
}

#[test]
fn empty_and_blank_documents_yield_nothing() {
    for text in ["", "\n\n  \n", "   "] {
        let layout = segment(text, Lang::Markdown, DocKind::Note, "n", &opts(1800, 240));
        assert!(layout.passages.is_empty(), "{text:?}");
    }
}

#[test]
fn minified_lines_are_hard_split() {
    let text = format!("var a={};\n", "x".repeat(1000));
    let layout = segment(
        &text,
        Lang::JavaScript,
        DocKind::File,
        "m.js",
        &opts(300, 0),
    );
    assert!(layout.passages.len() >= 4);
    check_invariants(&text, &layout, 300);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn segmentation_invariants_hold_for_arbitrary_text(
        lines in proptest::collection::vec("[ -~é検{}()#`\"'/*\\t]{0,60}", 0..60),
        lang_idx in 0usize..6,
        max in 128usize..900,
    ) {
        let text = lines.join("\n");
        let lang = [Lang::Rust, Lang::Python, Lang::Markdown, Lang::Text, Lang::JavaScript, Lang::Yaml][lang_idx];
        let layout = segment(&text, lang, DocKind::File, "p", &opts(max, max / 4));
        check_invariants(&text, &layout, max.max(128));
        let n = Lines::new(&text).len() as u32;
        for item in outline(&text, lang) {
            prop_assert!(1 <= item.lines.0 && item.lines.0 <= item.lines.1 && item.lines.1 <= n, "{item:?} of {n}");
        }
    }
}

fn outlined(text: &str, lang: Lang) -> Vec<(usize, String, (u32, u32))> {
    outline(text, lang)
        .into_iter()
        .map(|i| (i.depth, i.name, i.lines))
        .collect()
}

#[test]
fn outlines_list_every_definition_whatever_the_passage_sizes() {
    let rust = "/// Totals.\npub struct Ledger {\n    total: u64,\n}\n\nimpl Default for Ledger {\n    fn default() -> Self {\n        Ledger { total: 0 }\n    }\n}\n\nimpl Ledger {\n    /// Add.\n    pub fn credit(&mut self, n: u64) {\n        self.total += n;\n    }\n\n    pub fn debit(&mut self, n: u64) {\n        self.total -= n;\n    }\n}\n\npub const A: u32 = 1;\npub const B: u32 = 2;\n";
    assert_eq!(
        outlined(rust, Lang::Rust),
        [
            (0, "struct Ledger".to_string(), (1, 4)),
            (0, "impl Default for Ledger".to_string(), (6, 10)),
            (1, "method default".to_string(), (7, 10)),
            (0, "impl Ledger".to_string(), (12, 21)),
            (1, "method credit".to_string(), (13, 16)),
            (1, "method debit".to_string(), (18, 21)),
            (0, "constant A".to_string(), (23, 23)),
            (0, "constant B".to_string(), (24, 24)),
        ]
    );
    let python = "class Queue:\n    def push(self, x):\n        self.items.append(x)\n\n    def pop(self):\n        return self.items.pop()\n\n\ndef helper():\n    return Queue()\n";
    let names: Vec<(usize, String)> = outlined(python, Lang::Python)
        .into_iter()
        .map(|(d, n, _)| (d, n))
        .collect();
    assert_eq!(
        names,
        [
            (0, "class Queue".to_string()),
            (1, "method push".to_string()),
            (1, "method pop".to_string()),
            (0, "function helper".to_string()),
        ]
    );
}

#[test]
fn markdown_outlines_nest_headings_with_their_line_ranges() {
    let md = "# Guide\n\nIntro.\n\n## Setup\n\n### macOS\n\nbrew install x\n\n## Use\n\nRun it.\n";
    assert_eq!(
        outlined(md, Lang::Markdown),
        [
            (0, "Guide".to_string(), (1, 13)),
            (1, "Setup".to_string(), (5, 10)),
            (2, "macOS".to_string(), (7, 10)),
            (1, "Use".to_string(), (11, 13)),
        ]
    );
    assert!(outline("plain words\n", Lang::Text).is_empty());
    assert!(outline("", Lang::Rust).is_empty());
}
