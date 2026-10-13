//! Question analysis: terms, identifiers, phrases, entity keys, and intent.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::analysis::{analyze, fold_key, for_each_word, is_stopword, relation_class, term_set};

/// What a question asks for. Shifts structural priors: a *where* question
/// favours definitions, a *recall* question favours past conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryIntent {
    /// Find where something is defined or implemented.
    Locate,
    /// Understand how or why something works.
    Explain,
    /// A factual question about people, organizations, dates, quantities.
    Fact,
    /// Recall something from earlier conversation.
    Recall,
    /// None of the above.
    General,
}

/// The relations a question asks about: its terms, and their relation
/// classes, so a `started` edge answers "who founded".
pub(crate) struct QuestionRelations {
    terms: HashSet<String>,
    classes: HashSet<&'static str>,
}

impl QuestionRelations {
    pub(crate) fn new(query: &AnalyzedQuery) -> Self {
        let terms: HashSet<String> = query.content_terms.iter().cloned().collect();
        let classes = terms.iter().filter_map(|t| relation_class(t)).collect();
        QuestionRelations { terms, classes }
    }

    /// Whether `predicate` names a relation the question asks about.
    pub(crate) fn matches(&self, predicate: &str) -> bool {
        !self.matched(predicate).is_empty()
    }

    /// The parts of the question `predicate` answers: relation classes
    /// where the term has one (`started` and `founded` are both `found`),
    /// question terms otherwise.
    pub(crate) fn matched(&self, predicate: &str) -> Vec<String> {
        let mut out = Vec::new();
        for t in term_set(predicate) {
            match relation_class(&t) {
                Some(c) if self.classes.contains(c) => out.push(c.to_string()),
                _ if self.terms.contains(&t) => out.push(t),
                _ => {}
            }
        }
        out
    }
}

/// A question, analyzed.
#[derive(Debug, Clone, Default)]
pub(crate) struct AnalyzedQuery {
    /// Distinct normalized non-compound terms, in question order.
    pub content_terms: Vec<String>,
    /// Distinct compound (identifier) terms.
    pub compounds: Vec<String>,
    /// Lowercased exact strings: quoted/backticked spans and identifiers.
    pub phrases: Vec<String>,
    /// Identifiers as written, for exact search in a syntext index.
    pub identifiers: Vec<String>,
    /// Canonical keys of word n-grams and identifiers, for graph seeds.
    pub entity_keys: Vec<String>,
    pub intent: Option<QueryIntent>,
}

impl AnalyzedQuery {
    /// Content terms followed by compounds.
    pub(crate) fn all_terms(&self) -> impl Iterator<Item = &String> {
        self.content_terms.iter().chain(self.compounds.iter())
    }
}

/// Longest n-gram, in words, tried as an entity key.
const MAX_NGRAM: usize = 4;

pub(crate) fn analyze_query(question: &str) -> AnalyzedQuery {
    let mut q = AnalyzedQuery::default();
    for t in analyze(question) {
        let list = if t.compound {
            &mut q.compounds
        } else {
            &mut q.content_terms
        };
        if !list.contains(&t.text) {
            list.push(t.text);
        }
    }
    for span in delimited_spans(question) {
        push_unique(&mut q.phrases, span.to_lowercase());
        if span.len() >= 3 && !span.contains(char::is_whitespace) {
            push_unique(&mut q.identifiers, span.to_string());
        }
    }
    for token in question.split_whitespace() {
        let t = token.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        if t.len() >= 3 && is_identifier(t) {
            push_unique(&mut q.identifiers, t.to_string());
            push_unique(&mut q.phrases, t.to_lowercase());
        }
    }
    q.entity_keys = entity_keys(question, &q.identifiers);
    q.intent = Some(detect_intent(question));
    q
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

/// Contents of `"..."`, `'...'` (when it spans words), and `` `...` ``.
fn delimited_spans(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for delim in ['"', '`', '\u{201C}'] {
        let close = if delim == '\u{201C}' {
            '\u{201D}'
        } else {
            delim
        };
        let mut rest = text;
        while let Some(start) = rest.find(delim) {
            let after = &rest[start + delim.len_utf8()..];
            let Some(end) = after.find(close) else {
                break;
            };
            let span = after[..end].trim();
            if !span.is_empty() && span.len() <= 200 {
                out.push(span);
            }
            rest = &after[end + close.len_utf8()..];
        }
    }
    out
}

/// Code-shaped tokens: snake_case, camelCase, `a::b`, `a.b`, paths.
fn is_identifier(t: &str) -> bool {
    let has_alpha = t.chars().any(char::is_alphabetic);
    let snake = t.contains('_');
    let dotted = t.contains('.') && t.split('.').all(|s| s.len() >= 2);
    let path = t.contains("::") || t.contains('/') || dotted;
    let camel = t
        .chars()
        .zip(t.chars().skip(1))
        .any(|(a, b)| a.is_lowercase() && b.is_uppercase());
    has_alpha && (snake || path || camel)
}

/// Canonical keys for every 1..=4-word n-gram that is not all stopwords,
/// plus folded identifiers. Longer n-grams come first.
fn entity_keys(question: &str, identifiers: &[String]) -> Vec<String> {
    let mut words: Vec<&str> = Vec::new();
    for_each_word(question, |w, _| words.push(w));
    let mut keys: Vec<String> = Vec::new();
    for n in (1..=MAX_NGRAM).rev() {
        for window in words.windows(n) {
            if window
                .iter()
                .all(|w| is_stopword(&w.to_lowercase()) || w.len() < 2)
            {
                continue;
            }
            // An n-gram may not start or end on a stopword ("of the").
            let edge_stop = |w: &str| is_stopword(&w.to_lowercase());
            if n > 1 && (edge_stop(window[0]) || edge_stop(window[n - 1])) {
                continue;
            }
            push_unique(&mut keys, fold_key(&window.join(" ")));
        }
    }
    for ident in identifiers {
        push_unique(&mut keys, fold_key(ident));
    }
    keys
}

fn detect_intent(question: &str) -> QueryIntent {
    let q = format!(" {} ", question.to_lowercase());
    let any = |cues: &[&str]| cues.iter().any(|c| q.contains(c));
    if any(&[
        " earlier",
        " previously",
        " last time",
        " you said",
        " we discussed",
        " we talked",
        " i told you",
        " i mentioned",
        " did i ",
        " did we ",
        " remind me",
        " what was my ",
        " our conversation",
        " you suggested",
        " you recommended",
    ]) {
        QueryIntent::Recall
    } else if any(&[
        " where is ",
        " where's ",
        " where are ",
        " which file",
        " what file",
        " defined",
        " definition of ",
        " implemented",
        " implementation of ",
        " located",
        " location of ",
        " find the ",
    ]) {
        QueryIntent::Locate
    } else if any(&[
        " how does ",
        " how do ",
        " how is ",
        " how are ",
        " why ",
        " explain",
        " purpose of ",
        " overview",
        " architecture",
        " design of ",
        " walk me through",
        " what happens when ",
    ]) {
        QueryIntent::Explain
    } else if any(&[
        " who ",
        " whom ",
        " whose ",
        " when ",
        " what year",
        " how many ",
        " how much ",
        " which company",
        " which person",
        " what date",
    ]) {
        QueryIntent::Fact
    } else {
        QueryIntent::General
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_terms_identifiers_and_phrases() {
        let q = analyze_query("Where is `parse_query` defined and what calls Index::search?");
        assert!(q.compounds.contains(&"parsequery".to_string()));
        assert!(q.content_terms.contains(&"pars".to_string()));
        assert!(q.identifiers.contains(&"parse_query".to_string()));
        assert!(q.identifiers.contains(&"Index::search".to_string()));
        assert!(q.phrases.contains(&"parse_query".to_string()));
        assert!(q.entity_keys.contains(&"parse query".to_string()));
        assert!(q.entity_keys.contains(&"index search".to_string()));
        assert_eq!(q.intent, Some(QueryIntent::Locate));
    }

    #[test]
    fn entity_keys_cover_multiword_names_and_possessives() {
        let q = analyze_query("Who founded the company acquired by Alice's employer?");
        assert!(
            q.entity_keys.contains(&"alice".to_string()),
            "{:?}",
            q.entity_keys
        );
        assert!(!q.entity_keys.contains(&"the".to_string()));
        assert!(!q
            .entity_keys
            .iter()
            .any(|k| k.ends_with(" the") || k.starts_with("the ")));
        assert_eq!(q.intent, Some(QueryIntent::Fact));
        let q = analyze_query("what does Meridian Labs own");
        assert!(q.entity_keys.contains(&"meridian labs".to_string()));
    }

    #[test]
    fn intents() {
        let cases = [
            (
                "What did I say earlier about the deploy key?",
                QueryIntent::Recall,
            ),
            ("How does the overlay get rebuilt?", QueryIntent::Explain),
            ("Which file handles locking?", QueryIntent::Locate),
            ("When was the release cut?", QueryIntent::Fact),
            ("list the config options", QueryIntent::General),
        ];
        for (q, intent) in cases {
            assert_eq!(analyze_query(q).intent, Some(intent), "{q}");
        }
    }

    #[test]
    fn plain_words_are_not_identifiers() {
        let q = analyze_query("Explain the build pipeline, e.g. stages.");
        assert!(q.identifiers.is_empty(), "{:?}", q.identifiers);
        let q = analyze_query("open src/index/mod.rs and parseQuery");
        assert!(q.identifiers.contains(&"src/index/mod.rs".to_string()));
        assert!(q.identifiers.contains(&"parseQuery".to_string()));
    }
}
