//! Sentence splitting and entity-span detection for prose.

use std::sync::OnceLock;

use regex::Regex;

use super::{entity, AliasFact, EntityKind, EntityRef, PassageFacts};
use crate::analysis::is_stopword;

/// Longest entity span, in words.
const MAX_SPAN_WORDS: usize = 6;
/// Capitalized words that start sentences without naming anything. Sorted.
const COMMON_STARTERS: &[&str] = &[
    "add",
    "additionally",
    "afterwards",
    "again",
    "although",
    "another",
    "anyway",
    "call",
    "check",
    "consider",
    "create",
    "currently",
    "earlier",
    "eventually",
    "example",
    "finally",
    "first",
    "hence",
    "initially",
    "install",
    "instead",
    "later",
    "let",
    "make",
    "many",
    "meanwhile",
    "next",
    "note",
    "now",
    "one",
    "originally",
    "overall",
    "previously",
    "recently",
    "returns",
    "run",
    "second",
    "see",
    "several",
    "since",
    "still",
    "third",
    "today",
    "together",
    "usage",
    "use",
    "yes",
    "yesterday",
];

/// Words joining capitalized words inside one name ("Bank of America").
const CONNECTORS: &[&str] = &[
    "&", "and", "da", "de", "del", "di", "du", "for", "la", "le", "of", "the", "van", "von",
];

#[derive(Debug)]
pub(super) struct Span {
    pub start: usize,
    pub end: usize,
    pub entity: EntityRef,
}

/// Split into sentences at `.`, `!`, `?` followed by whitespace and a
/// capital/digit, and at blank lines; abbreviations and initials do not end
/// sentences.
pub(super) fn sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let para_break = c == b'\n' && bytes.get(i + 1) == Some(&b'\n');
        let terminal = matches!(c, b'.' | b'!' | b'?')
            && bytes.get(i + 1).is_some_and(|b| b.is_ascii_whitespace())
            && next_starts_sentence(&text[i + 1..])
            && !is_abbreviation(&text[start..i]);
        if para_break || terminal {
            push_sentence(text, start, i + 1, &mut out);
            start = i + 1;
        }
        i += 1;
    }
    push_sentence(text, start, text.len(), &mut out);
    out
}

fn push_sentence<'a>(text: &'a str, s: usize, e: usize, out: &mut Vec<&'a str>) {
    let sentence = &text[s..e];
    if !sentence.trim().is_empty() {
        out.push(sentence);
    }
}

fn next_starts_sentence(rest: &str) -> bool {
    rest.trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_uppercase() || c.is_ascii_digit() || c == '`' || c == '"')
}

/// True when the text before a period ends in an abbreviation or initial.
fn is_abbreviation(before: &str) -> bool {
    let word = before
        .rsplit(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let single_initial = word.len() == 1 && word.chars().all(|c| c.is_alphabetic());
    single_initial
        || matches!(
            word.as_str(),
            "e.g"
                | "i.e"
                | "etc"
                | "vs"
                | "mr"
                | "mrs"
                | "ms"
                | "dr"
                | "prof"
                | "inc"
                | "ltd"
                | "co"
                | "corp"
                | "jr"
                | "sr"
                | "st"
                | "no"
                | "fig"
                | "approx"
                | "cf"
                | "al"
        )
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TokKind {
    Cap,
    Acronym,
    Code,
    Connector,
    Other,
}

/// Entity spans in one sentence, in order.
pub(super) fn entity_spans(sentence: &str) -> Vec<Span> {
    let toks = tokens(sentence);
    let mut spans = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let (s, e, kind) = toks[i];
        match kind {
            TokKind::Code => {
                if let Some(ent) = entity(&sentence[s..e], EntityKind::Symbol) {
                    spans.push(Span {
                        start: s,
                        end: e,
                        entity: ent,
                    });
                }
                i += 1;
            }
            TokKind::Cap | TokKind::Acronym => {
                let first_word = sentence[s..e].to_lowercase();
                let common = is_stopword(&first_word)
                    || COMMON_STARTERS.binary_search(&first_word.as_str()).is_ok();
                if i == 0 && common {
                    i += 1;
                    continue;
                }
                if kind == TokKind::Acronym && is_stopword(&first_word) {
                    i += 1;
                    continue;
                }
                // Extend over capitalized words and inner connectors that are
                // separated by exactly one space ("Bank of America").
                let mut last = i;
                let mut j = i + 1;
                while j < toks.len() && j - i < MAX_SPAN_WORDS * 2 {
                    if toks[j].0 != toks[j - 1].1 + 1 {
                        break;
                    }
                    let next_is_name = toks
                        .get(j + 1)
                        .is_some_and(|t| matches!(t.2, TokKind::Cap | TokKind::Acronym));
                    match toks[j].2 {
                        TokKind::Cap | TokKind::Acronym => last = j,
                        TokKind::Connector if next_is_name => {}
                        _ => break,
                    }
                    j += 1;
                }
                let (start, end) = (s, toks[last].1);
                let words = last - i + 1;
                if words <= MAX_SPAN_WORDS {
                    let name = strip_possessive(&sentence[start..end]);
                    if let Some(ent) = entity(name, EntityKind::Name) {
                        spans.push(Span {
                            start,
                            end,
                            entity: ent,
                        });
                    }
                }
                i = last + 1;
            }
            _ => i += 1,
        }
    }
    spans
}

fn strip_possessive(s: &str) -> &str {
    s.strip_suffix("'s")
        .or_else(|| s.strip_suffix("\u{2019}s"))
        .unwrap_or(s)
}

/// Word tokens `(start, end, kind)` with surrounding punctuation trimmed.
fn tokens(sentence: &str) -> Vec<(usize, usize, TokKind)> {
    let mut out = Vec::new();
    let mut i = 0;
    let bytes = sentence.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'`' {
            if let Some(close) = sentence[i + 1..].find('`') {
                let (s, e) = (i + 1, i + 1 + close);
                if close > 0 && close <= 64 {
                    out.push((s, e, TokKind::Code));
                }
                i = e + 1;
                continue;
            }
            // Unmatched backtick (a code span cut by sentence splitting):
            // skip it, or the word scan below would never advance.
            i += 1;
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let s = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'`' {
            i += 1;
        }
        let raw = &sentence[s..i];
        let trimmed = raw.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == '&'));
        if trimmed.is_empty() {
            continue;
        }
        let off = s + raw.find(trimmed).unwrap_or(0);
        let end = off + trimmed.len();
        out.push((off, end, classify(&sentence[off..end])));
    }
    out
}

fn classify(word: &str) -> TokKind {
    let lower = word.to_lowercase();
    if CONNECTORS.contains(&lower.as_str()) {
        return TokKind::Connector;
    }
    let code_like = word.contains('_')
        || word.contains("::")
        || word.chars().skip(1).any(char::is_uppercase) && word.chars().any(char::is_lowercase);
    let first = word.chars().next().unwrap_or(' ');
    let letters = word.chars().filter(|c| c.is_alphabetic()).count();
    if word.len() >= 2
        && letters >= 2
        && word.len() <= 6
        && word.chars().all(|c| c.is_uppercase() || c.is_ascii_digit())
    {
        TokKind::Acronym
    } else if code_like && letters >= 2 {
        TokKind::Code
    } else if first.is_uppercase() {
        TokKind::Cap
    } else {
        TokKind::Other
    }
}

/// "Abstract Syntax Tree (AST)" and "AST (Abstract Syntax Tree)".
pub(super) fn acronym_aliases(sentence: &str, facts: &mut PassageFacts) {
    static LONG_FIRST: OnceLock<Regex> = OnceLock::new();
    static SHORT_FIRST: OnceLock<Regex> = OnceLock::new();
    let long_first = LONG_FIRST.get_or_init(|| {
        Regex::new(r"((?:[A-Z][\w-]*\s+(?:(?:of|and|for|the|&)\s+)?){1,7}[A-Z][\w-]*)\s*\(([A-Z][A-Za-z0-9]{1,7})\)")
            .expect("acronym pattern")
    });
    let short_first = SHORT_FIRST.get_or_init(|| {
        Regex::new(r"\b([A-Z][A-Z0-9]{1,7})\s*\(((?:[A-Z][\w-]*\s+(?:(?:of|and|for|the|&)\s+)?){1,7}[A-Z][\w-]*)\)")
            .expect("acronym pattern")
    });
    for c in long_first.captures_iter(sentence) {
        push_acronym(&c[1], &c[2], facts);
    }
    for c in short_first.captures_iter(sentence) {
        push_acronym(&c[2], &c[1], facts);
    }
}

fn push_acronym(long: &str, short: &str, facts: &mut PassageFacts) {
    let words: Vec<&str> = long
        .split_whitespace()
        .filter(|w| !CONNECTORS.contains(&w.to_lowercase().as_str()))
        .collect();
    // Allow a leading word or two before the expansion proper.
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    let short_up = short.to_uppercase();
    let Some(pos) = initials.to_uppercase().rfind(&short_up) else {
        return;
    };
    if pos + short_up.len() != initials.len() {
        return;
    }
    let expansion = words[pos..].join(" ");
    if let (Some(a), Some(t)) = (
        entity(short, EntityKind::Name),
        entity(&expansion, EntityKind::Name),
    ) {
        facts.aliases.push(AliasFact {
            alias: a.key,
            target: t.key,
            confidence: 0.95,
        });
    }
}

#[cfg(test)]
pub(super) fn starters() -> &'static [&'static str] {
    COMMON_STARTERS
}
