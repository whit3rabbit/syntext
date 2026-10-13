use super::*;

fn span(lines: &Lines<'_>, l0: usize, l1: usize, kind: PassageKind) -> PassageSpan {
    PassageSpan::from_lines(lines, l0, l1, kind)
}

fn opts(max: usize, min: usize) -> SegmentOptions {
    SegmentOptions {
        max_passage_bytes: max,
        min_passage_bytes: min,
    }
}

#[test]
fn splits_prefer_blank_lines_in_the_back_half() {
    let text = "aaaaaaaaa\nbbbbbbbbb\n\nccccccccc\nddddddddd\n";
    let lines = Lines::new(text);
    let mut layout = Layout {
        sections: vec![],
        passages: vec![span(&lines, 0, 5, PassageKind::Prose)],
    };
    normalize(&mut layout, &lines, text, &opts(35, 0));
    let parts: Vec<&str> = layout
        .passages
        .iter()
        .map(|p| text[p.start..p.end].trim())
        .collect();
    assert_eq!(parts, ["aaaaaaaaa\nbbbbbbbbb", "ccccccccc\nddddddddd"]);
}

#[test]
fn hard_split_respects_char_boundaries() {
    let text = "é".repeat(100);
    let mut out = Vec::new();
    hard_split(&text, 0, text.len(), 7, &mut out);
    assert!(out
        .iter()
        .all(|&(a, b)| text.is_char_boundary(a) && text.is_char_boundary(b)));
    assert_eq!(out.last().unwrap().1, text.len());
    let mut out = Vec::new();
    hard_split("é", 0, 2, 1, &mut out);
    assert_eq!(out, [(0, 2)], "budget below one char still progresses");
}

#[test]
fn definition_kind_stays_with_the_signature_chunk() {
    let text = "fn big() {\n    aaaaaaaaaaaaaaaaaaaa();\n    bbbbbbbbbbbbbbbbbbbb();\n}\n";
    let lines = Lines::new(text);
    let mut p = span(&lines, 0, 4, PassageKind::Definition);
    p.symbol = Some(super::super::Symbol {
        name: "big".into(),
        kind: SymbolKind::Function,
    });
    p.anchor_len = 1;
    let mut layout = Layout {
        sections: vec![],
        passages: vec![p],
    };
    normalize(&mut layout, &lines, text, &opts(40, 0));
    assert!(layout.passages.len() >= 2);
    assert_eq!(layout.passages[0].kind, PassageKind::Definition);
    assert!(layout.passages[1..]
        .iter()
        .all(|p| p.kind == PassageKind::Code && p.symbol.is_none() && p.anchor_len == 0));
}

#[test]
fn merges_only_compatible_neighbours() {
    let text = "a\nb\nc\nd\n";
    let lines = Lines::new(text);
    let mut prose = span(&lines, 0, 1, PassageKind::Prose);
    prose.section = Some(0);
    let mut other_section = span(&lines, 1, 2, PassageKind::Prose);
    other_section.section = Some(1);
    let code_a = span(&lines, 2, 3, PassageKind::Code);
    let code_b = span(&lines, 3, 4, PassageKind::Code);
    let mut layout = Layout {
        sections: vec![],
        passages: vec![prose, other_section, code_a, code_b],
    };
    normalize(&mut layout, &lines, text, &opts(1800, 240));
    assert_eq!(layout.passages.len(), 3);
    assert_eq!(layout.passages[2].line_start, 3);
    assert_eq!(layout.passages[2].line_end, 4);
}
