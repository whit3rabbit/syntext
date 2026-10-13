use super::*;
use crate::structure::{segment, SegmentOptions};
use crate::types::DocKind;

fn facts_for(text: &str, lang: Lang, mode: GraphMode) -> Vec<PassageFacts> {
    let layout = segment(text, lang, DocKind::File, "f", &SegmentOptions::default());
    layout
        .passages
        .iter()
        .map(|p| extract(text, p, lang, mode))
        .collect()
}

fn merged(text: &str, lang: Lang, mode: GraphMode) -> PassageFacts {
    let mut all = PassageFacts::default();
    for f in facts_for(text, lang, mode) {
        all.defines.extend(f.defines);
        all.mentions.extend(f.mentions);
        all.aliases.extend(f.aliases);
        all.edges.extend(f.edges);
    }
    all
}

fn has_edge(f: &PassageFacts, s: &str, pred: &str, o: &str) -> bool {
    f.edges
        .iter()
        .any(|e| e.subject.key == s && e.predicate == pred && e.object.key == o)
}

fn has_alias(f: &PassageFacts, alias: &str, target: &str) -> bool {
    f.aliases
        .iter()
        .any(|a| a.alias == alias && a.target == target)
}

#[test]
fn off_mode_extracts_nothing() {
    let f = merged("fn a() { b(); }", Lang::Rust, GraphMode::Off);
    assert!(f.is_empty());
}

#[test]
fn rust_definitions_calls_and_containment() {
    let mut text = String::from("impl Index {\n");
    for (name, callee) in [("search", "route_query"), ("open", "read_manifest")] {
        text.push_str(&format!(
            "    pub fn {name}(&self) -> usize {{\n        let x = {callee}(self.items.len());\n        x.unwrap()\n    }}\n\n"
        ));
    }
    text.push_str(&"    // padding\n".repeat(120));
    text.push_str("}\n");
    let f = merged(&text, Lang::Rust, GraphMode::Code);
    let defined: Vec<&str> = f.defines.iter().map(|e| e.key.as_str()).collect();
    assert!(
        defined.contains(&"search") && defined.contains(&"open"),
        "{defined:?}"
    );
    assert!(
        f.mentions.iter().any(|e| e.key == "index"),
        "impl target is a mention"
    );
    assert!(has_edge(&f, "search", "member of", "index"));
    assert!(has_edge(&f, "search", "calls", "route query"));
    assert!(has_edge(&f, "open", "calls", "read manifest"));
    assert!(!f
        .edges
        .iter()
        .any(|e| e.object.key == "unwrap" || e.object.key == "len"));
}

#[test]
fn inheritance_across_languages() {
    let f = merged(
        "class Child(Base, Mixin):\n    pass\n",
        Lang::Python,
        GraphMode::Code,
    );
    assert!(has_edge(&f, "child", "extends", "base"));
    assert!(has_edge(&f, "child", "extends", "mixin"));
    let f = merged(
        "public class Repo extends BaseRepo implements Store, Closeable {\n}\n",
        Lang::Java,
        GraphMode::Code,
    );
    assert!(has_edge(&f, "repo", "extends", "base repo"));
    assert!(has_edge(&f, "repo", "implements", "store"));
    assert!(has_edge(&f, "repo", "implements", "closeable"));
    let f = merged("impl Display for Key {\n}\n", Lang::Rust, GraphMode::Code);
    assert!(has_edge(&f, "key", "implements", "display"));
    let f = merged(
        "pub trait Store: Send + Sync {\n}\n",
        Lang::Rust,
        GraphMode::Code,
    );
    assert!(has_edge(&f, "store", "extends", "send"));
}

#[test]
fn import_and_type_aliases() {
    let f = merged(
        "import numpy as np\nfrom a.b import Thing as T2\n",
        Lang::Python,
        GraphMode::Code,
    );
    assert!(has_alias(&f, "np", "numpy"));
    assert!(has_alias(&f, "t2", "thing"));
    let f = merged(
        "use crate::index::Index as Idx;\ntype Res = std::io::Result;\n",
        Lang::Rust,
        GraphMode::Code,
    );
    assert!(has_alias(&f, "idx", "index"));
    assert!(has_alias(&f, "res", "result"));
    let f = merged(
        "import * as path from 'node:path';\nimport { readFile as rf } from 'fs';\n",
        Lang::TypeScript,
        GraphMode::Code,
    );
    assert!(has_alias(&f, "path", "path") || f.aliases.iter().all(|a| a.alias != a.target));
    assert!(has_alias(&f, "rf", "read file"));
}

#[test]
fn code_mode_skips_prose() {
    let f = merged(
        "Alice works for Meridian Labs.",
        Lang::Markdown,
        GraphMode::Code,
    );
    assert!(f.is_empty());
}

#[test]
fn prose_relations_for_multi_hop_chains() {
    let text = "Alice works for Meridian Labs.\n\nMeridian Labs acquired Northstar Systems in 2021.\n\nNorthstar Systems was founded by Robert.\n";
    let f = merged(text, Lang::Markdown, GraphMode::Full);
    assert!(
        has_edge(&f, "alice", "works for", "meridian labs"),
        "{:?}",
        f.edges
    );
    assert!(has_edge(
        &f,
        "meridian labs",
        "acquired",
        "northstar systems"
    ));
    assert!(
        has_edge(&f, "robert", "founded", "northstar systems"),
        "passive normalized"
    );
    let mentioned: Vec<&str> = f.mentions.iter().map(|e| e.key.as_str()).collect();
    for key in ["alice", "meridian labs", "northstar systems", "robert"] {
        assert!(mentioned.contains(&key), "{key} not in {mentioned:?}");
    }
}

#[test]
fn acronyms_and_aka_become_aliases() {
    let f = merged(
        "The parser builds an Abstract Syntax Tree (AST) first. The EU (European Union) agreed.",
        Lang::Text,
        GraphMode::Full,
    );
    assert!(
        has_alias(&f, "ast", "abstract syntax tree"),
        "{:?}",
        f.aliases
    );
    assert!(has_alias(&f, "eu", "european union"));
    let f = merged(
        "Northstar Systems, formerly Polaris Labs, ships routers.",
        Lang::Text,
        GraphMode::Full,
    );
    assert!(
        has_alias(&f, "polaris labs", "northstar systems"),
        "{:?}",
        f.aliases
    );
}

#[test]
fn pronouns_carry_the_previous_subject_with_lower_confidence() {
    let f = merged(
        "Dana leads Platform. She approved Project Atlas.",
        Lang::Text,
        GraphMode::Full,
    );
    let edge = f
        .edges
        .iter()
        .find(|e| e.subject.key == "dana" && e.object.key == "project atlas")
        .expect("pronoun edge");
    assert!(edge.confidence < 0.7, "{}", edge.confidence);
}

#[test]
fn sentence_starters_and_abbreviations() {
    let f = merged(
        "The system uses e.g. Redis. Returns nothing.",
        Lang::Text,
        GraphMode::Full,
    );
    let mentioned: Vec<&str> = f.mentions.iter().map(|e| e.key.as_str()).collect();
    assert!(!mentioned.contains(&"the"));
    assert!(!mentioned.contains(&"returns"));
    assert!(mentioned.contains(&"redis"));
}

#[test]
fn code_tokens_in_prose_are_symbols() {
    let f = merged(
        "Call `ContextStore::open` before parse_query runs.",
        Lang::Markdown,
        GraphMode::Full,
    );
    let symbols: Vec<&str> = f
        .mentions
        .iter()
        .filter(|e| e.kind == EntityKind::Symbol)
        .map(|e| e.key.as_str())
        .collect();
    assert!(symbols.contains(&"context store open"), "{symbols:?}");
    assert!(symbols.contains(&"parse query"), "{symbols:?}");
}

#[test]
fn facts_are_deduplicated() {
    let f = facts_for("Alice met Bob. Alice met Bob.", Lang::Text, GraphMode::Full);
    let f = &f[0];
    let mut keys: Vec<&str> = f.mentions.iter().map(|e| e.key.as_str()).collect();
    let n = keys.len();
    keys.dedup();
    assert_eq!(n, keys.len());
}

#[test]
fn lexicons_are_sorted() {
    for list in [
        prose::lexicons().0,
        prose::lexicons().1,
        code::callee_stoplist(),
    ] {
        assert!(
            list.windows(2).all(|w| w[0] < w[1]),
            "{:?}",
            list.windows(2).find(|w| w[0] >= w[1])
        );
    }
}

#[test]
fn unmatched_backticks_terminate() {
    // A code span cut in half by sentence splitting used to stall the
    // prose tokenizer forever.
    let text = "Call `parse_query. Then it runs. Stray ` and `` and `a`b`.\n";
    let f = merged(text, Lang::Markdown, GraphMode::Full);
    assert!(
        f.mentions.iter().any(|e| e.key == "parse query"),
        "identifier after a stray backtick still found: {:?}",
        f.mentions
    );
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

    #[test]
    fn extraction_terminates_on_arbitrary_text(
        text in "[ -~`'\u{2019}\n\t]{0,300}",
        lang_idx in 0usize..4,
    ) {
        let lang = [Lang::Markdown, Lang::Text, Lang::Rust, Lang::Python][lang_idx];
        let _ = merged(&text, lang, GraphMode::Full);
    }
}

#[test]
fn verbs_are_matched_by_their_real_stems() {
    // `employs` stems to `emploi` and `buys` to `bui`: both must still read
    // as relations, not bare co-mentions.
    let f = merged(
        "Juniper Logistics employs Amara Nakamura. Kestrel Foods buys Copperleaf Foods.\n",
        Lang::Markdown,
        GraphMode::Full,
    );
    assert!(has_edge(
        &f,
        "juniper logistics",
        "employs",
        "amara nakamura"
    ));
    assert!(has_edge(&f, "kestrel foods", "buys", "copperleaf foods"));
}

#[test]
fn a_heading_that_is_a_name_defines_that_entity() {
    let f = merged(
        "# Juniper Logistics\n\nJuniper Logistics was founded by Zoe Sato in 1981.\n",
        Lang::Markdown,
        GraphMode::Full,
    );
    assert!(f.defines.iter().any(|e| e.key == "juniper logistics"));
    for heading in ["# Notes on Juniper Logistics", "# Refund policy"] {
        let f = merged(
            &format!("{heading}\n\nJuniper Logistics ships parcels.\n"),
            Lang::Markdown,
            GraphMode::Full,
        );
        assert!(f.defines.is_empty(), "{heading}: {:?}", f.defines);
    }
}
