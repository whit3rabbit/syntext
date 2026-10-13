use super::*;

fn layout(text: &str, max: usize) -> Layout {
    let lines = Lines::new(text);
    markdown(
        &lines,
        "d.md",
        &SegmentOptions {
            max_passage_bytes: max,
            min_passage_bytes: 0,
        },
    )
}

#[test]
fn headings_parse_atx_and_setext() {
    assert_eq!(
        atx_heading("## Install ##"),
        Some((2, "Install".to_string()))
    );
    assert_eq!(atx_heading("#hashtag"), None);
    assert_eq!(atx_heading("    # indented code"), None);
    assert_eq!(atx_heading("#######"), None);
    assert_eq!(setext_level("====="), Some(1));
    assert_eq!(setext_level("---"), Some(2));
    assert_eq!(setext_level("-"), None);
    let l = layout("Title\n=====\n\nBody text.\n", 1800);
    assert_eq!(l.sections.len(), 1);
    assert_eq!(l.sections[0].title, "Title");
}

#[test]
fn fenced_code_hides_headings_and_blank_lines() {
    let text = "# A\n\n```sh\n# not a heading\n\necho hi\n```\n";
    let l = layout(text, 1800);
    assert_eq!(l.sections.len(), 1);
    assert_eq!(l.passages.len(), 1);
}

#[test]
fn code_heavy_passages_are_tagged_code() {
    let text = "```rust\nfn a() {}\nfn b() {}\nfn c() {}\n```\n";
    let l = layout(text, 1800);
    assert_eq!(l.passages[0].kind, PassageKind::Code);
}

#[test]
fn paragraphs_group_until_budget() {
    let para = "word ".repeat(30);
    let text = format!("{para}\n\n{para}\n\n{para}\n");
    let l = layout(&text, 340);
    assert_eq!(l.passages.len(), 2, "two paragraphs fit per passage");
}

#[test]
fn list_lines_are_not_setext_headings() {
    let text = "- item\n---\n";
    let l = layout(text, 1800);
    assert!(l.sections.is_empty());
}

#[test]
fn front_matter_is_its_own_block() {
    let text = "---\ntitle: x\n---\n# Real\n\nBody.\n";
    let l = layout(text, 1800);
    assert_eq!(l.sections.len(), 1);
    assert_eq!(l.passages.len(), 2);
}

#[test]
fn empty_heading_sections_produce_no_passage() {
    let text = "# A\n## B\n\nText.\n";
    let l = layout(text, 1800);
    assert_eq!(l.passages.len(), 1);
    assert_eq!(l.passages[0].breadcrumb, "d.md > A > B");
}
