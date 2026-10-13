//! Prose facts: named entities, aliases, and sentence relations.
//!
//! Deliberately lightweight (no parser, no model): entity spans are runs of
//! capitalized words, code tokens, and acronyms; a relation is the short verb
//! phrase between two spans in one sentence. Everything extracted carries a
//! confidence so traversal can prefer precise edges, and every edge keeps the
//! passage it came from, so a wrong edge costs ranking quality, never a
//! hallucinated fact: the model only ever sees the original passage.

use super::spans::{acronym_aliases, entity_spans, sentences, Span};
use super::{AliasFact, EdgeFact, EdgeKind, EntityRef, PassageFacts};
use crate::analysis::porter_stem;

/// Longest gap, in words, read as a predicate.
const MAX_PREDICATE_WORDS: usize = 6;
/// Weak co-mention edges recorded per sentence.
const MAX_COMENTIONS: usize = 4;

/// Porter stems of verbs that mark a gap as a relation rather than filler,
/// as `porter_stem` produces them (`employs` stems to `emploi`, `buys` to
/// `bui`). Sorted.
const VERBS: &[&str] = &[
    "acquir",
    "approv",
    "ar",
    "attend",
    "base",
    "be",
    "belong",
    "born",
    "bought",
    "bui",
    "build",
    "built",
    "call",
    "connect",
    "contain",
    "control",
    "creat",
    "defin",
    "depend",
    "describ",
    "design",
    "develop",
    "direct",
    "emploi",
    "employ",
    "establish",
    "extend",
    "found",
    "ha",
    "had",
    "handl",
    "head",
    "headquart",
    "hire",
    "implement",
    "includ",
    "invent",
    "is",
    "join",
    "launch",
    "lead",
    "led",
    "live",
    "locat",
    "made",
    "maintain",
    "make",
    "manag",
    "marri",
    "merg",
    "own",
    "partner",
    "produc",
    "provid",
    "publish",
    "purchas",
    "read",
    "receiv",
    "releas",
    "replac",
    "report",
    "requir",
    "return",
    "run",
    "sell",
    "send",
    "sign",
    "sold",
    "start",
    "store",
    "studi",
    "support",
    "us",
    "valid",
    "wa",
    "were",
    "won",
    "work",
    "write",
    "wrote",
];

/// Words dropped from predicates.
const FILLERS: &[&str] = &[
    "a",
    "also",
    "an",
    "currently",
    "eventually",
    "just",
    "later",
    "originally",
    "recently",
    "the",
    "then",
];

/// Leading words that resolve to the previous sentence's subject.
const PRONOUNS: &[&str] = &["he", "her", "his", "it", "its", "she", "their", "they"];

pub(super) fn extract(body: &str, facts: &mut PassageFacts) {
    if let Some(e) = heading_entity(body) {
        facts.defines.push(e);
    }
    let mut previous_subject: Option<EntityRef> = None;
    for sentence in sentences(body) {
        let spans = entity_spans(sentence);
        acronym_aliases(sentence, facts);
        for sp in &spans {
            facts.mentions.push(sp.entity.clone());
        }
        let mut subject_override: Option<(EntityRef, f32)> = None;
        if starts_with_pronoun(sentence, spans.first()) {
            if let Some(prev) = &previous_subject {
                subject_override = Some((prev.clone(), 0.6));
            }
        }
        relations(sentence, &spans, subject_override, facts);
        if let Some(first) = spans.first() {
            previous_subject = Some(first.entity.clone());
        }
    }
}

/// The entity a passage's leading Markdown heading names, when the heading
/// is nothing but that name (`# Juniper Logistics`): the section is about
/// it, so the passage defines it. Headings with other words (`# Refund
/// policy`, `# Notes on Juniper`) define nothing.
fn heading_entity(body: &str) -> Option<EntityRef> {
    let line = body.trim_start().lines().next()?;
    let text = line.trim_start_matches('#');
    if text.len() == line.len() || !text.starts_with(' ') {
        return None;
    }
    let text = text.trim().trim_end_matches(['.', ':']);
    let spans = entity_spans(text);
    match spans.as_slice() {
        [only] if only.start == 0 && only.end == text.len() => Some(only.entity.clone()),
        _ => None,
    }
}

fn starts_with_pronoun(sentence: &str, first_span: Option<&Span>) -> bool {
    let t = sentence.trim_start();
    let word: String = t
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect::<String>()
        .to_lowercase();
    let lead = sentence.len() - t.len();
    PRONOUNS.contains(&word.as_str()) && first_span.is_none_or(|sp| sp.start > lead)
}

fn relations(
    sentence: &str,
    spans: &[Span],
    subject_override: Option<(EntityRef, f32)>,
    facts: &mut PassageFacts,
) {
    let mut chain: Vec<(EntityRef, usize, usize, f32)> = Vec::new();
    if let Some((e, conf)) = subject_override {
        let lead = sentence.len() - sentence.trim_start().len();
        let pronoun_end = lead
            + sentence
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphabetic())
                .map(char::len_utf8)
                .sum::<usize>();
        chain.push((e, lead, pronoun_end, conf));
    }
    for sp in spans {
        chain.push((sp.entity.clone(), sp.start, sp.end, 1.0));
    }
    let mut comentions = 0;
    for w in chain.windows(2) {
        let (a, _, a_end, a_conf) = &w[0];
        let (b, b_start, _, b_conf) = &w[1];
        if a.key == b.key || a_end > b_start {
            continue;
        }
        let gap = &sentence[*a_end..*b_start];
        if is_aka(gap) {
            facts.aliases.push(AliasFact {
                alias: b.key.clone(),
                target: a.key.clone(),
                confidence: 0.8,
            });
            continue;
        }
        match predicate(gap) {
            Some((pred, passive, strength)) => {
                let (s, o) = if passive { (b, a) } else { (a, b) };
                facts.edges.push(EdgeFact {
                    subject: s.clone(),
                    predicate: pred,
                    object: o.clone(),
                    kind: EdgeKind::Relation,
                    confidence: strength * a_conf.min(*b_conf),
                });
            }
            None if comentions < MAX_COMENTIONS && !gap.contains([';', ':']) => {
                comentions += 1;
                facts.edges.push(EdgeFact {
                    subject: a.clone(),
                    predicate: "related to".to_string(),
                    object: b.clone(),
                    kind: EdgeKind::CoMention,
                    confidence: 0.2 * a_conf.min(*b_conf),
                });
            }
            None => {}
        }
    }
}

/// "also known as" style phrases marking the next span as an alias.
fn is_aka(gap: &str) -> bool {
    let g = gap.to_lowercase();
    let g = g.trim_matches(|c: char| !c.is_alphanumeric());
    matches!(
        g,
        "also known as"
            | "aka"
            | "a.k.a"
            | "formerly"
            | "formerly known as"
            | "or simply"
            | "short for"
    )
}

/// Read a relation predicate from the text between two spans: returns the
/// normalized predicate, whether it is passive ("was founded by"), and a
/// confidence.
fn predicate(gap: &str) -> Option<(String, bool, f32)> {
    if gap.contains([',', ';', ':', '(', ')', '\n']) {
        return None;
    }
    let words: Vec<String> = gap
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() || words.len() > MAX_PREDICATE_WORDS {
        return None;
    }
    const AUX: &[&str] = &[
        "is", "was", "were", "are", "been", "be", "being", "has", "had", "have",
    ];
    let participle = &words[words.len().saturating_sub(2)];
    let passive = words.len() >= 3
        && words.last().is_some_and(|w| w == "by")
        && AUX.contains(&words[0].as_str())
        && (participle.ends_with("ed") || participle.ends_with("en"));
    let kept: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !FILLERS.contains(w))
        .filter(|w| !(passive && (AUX.contains(w) || *w == "by")))
        .collect();
    if kept.is_empty() {
        return None;
    }
    let verbish = kept.iter().any(|w| {
        let stem = porter_stem(w);
        VERBS.binary_search(&stem.as_str()).is_ok() || (w.len() >= 4 && w.ends_with("ed"))
    });
    if !verbish {
        return None;
    }
    let strength = if kept
        .iter()
        .any(|w| VERBS.binary_search(&porter_stem(w).as_str()).is_ok())
    {
        0.7
    } else {
        0.45
    };
    let text = kept.join(" ");
    (text.len() <= 40).then_some((text, passive, strength))
}

#[cfg(test)]
pub(super) fn lexicons() -> (&'static [&'static str], &'static [&'static str]) {
    (VERBS, super::spans::starters())
}
