//! Text analysis: raw text to normalized index terms.
//!
//! The same analyzer runs when a document is compiled and when a question is
//! parsed, so both sides agree on what a term is. It is code-aware: an
//! identifier such as `parseQuery` or `parse_query` yields its subwords
//! (`pars`, `queri` after stemming) plus one *compound* term (`parsequery`)
//! shared by every casing convention, so `parse_query` in a question matches
//! `parseQuery` in a file.

mod relations;
mod stem;
mod stopwords;
mod words;

pub(crate) use relations::relation_class;
pub(crate) use stem::porter_stem;
pub(crate) use stopwords::is_stopword;
pub(crate) use words::{for_each_word, is_cjk, split_subwords};

/// Words longer than this are skipped (minified code, base64, data blobs).
const MAX_WORD_BYTES: usize = 64;
/// Pure-digit runs longer than this are skipped (timestamps, ids).
const MAX_NUMBER_BYTES: usize = 12;
/// Hex-only words at least this long are treated as hashes and skipped.
const MIN_HASH_BYTES: usize = 24;

/// One normalized term occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Term {
    /// Normalized term text.
    pub text: String,
    /// Token position. Subwords advance it; a compound term shares the
    /// position of its first subword. Dropped stopwords leave a gap.
    pub pos: u32,
    /// Byte range of the source text this term came from.
    pub start: usize,
    /// Exclusive end of that range.
    pub end: usize,
    /// True for the whole-identifier compound term.
    pub compound: bool,
}

/// Analyze `text` into terms in order of occurrence.
pub(crate) fn analyze(text: &str) -> Vec<Term> {
    let mut out = Vec::new();
    analyze_with(text, |t| out.push(t));
    out
}

/// Streaming form of [`analyze`].
pub(crate) fn analyze_with(text: &str, mut sink: impl FnMut(Term)) {
    let mut pos: u32 = 0;
    let mut subs: Vec<(usize, usize)> = Vec::new();
    for_each_word(text, |word, start| {
        if word.len() > MAX_WORD_BYTES || looks_like_hash(word) {
            pos = pos.saturating_add(1);
            return;
        }
        split_subwords(word, &mut subs);
        if subs.len() >= 2 {
            let compound = compound_of(word, &subs);
            if compound.len() >= 3 {
                sink(Term {
                    text: compound,
                    pos,
                    start,
                    end: start + word.len(),
                    compound: true,
                });
            }
        }
        for &(a, b) in &subs {
            if let Some(text) = normalize_token(&word[a..b]) {
                sink(Term {
                    text,
                    pos,
                    start: start + a,
                    end: start + b,
                    compound: false,
                });
            }
            pos = pos.saturating_add(1);
        }
    });
}

/// Normalize one subword: lowercase, drop stopwords and single letters,
/// stem ASCII words. Returns `None` when the token carries no signal.
pub(crate) fn normalize_token(raw: &str) -> Option<String> {
    let lower = if raw.is_ascii() {
        raw.to_ascii_lowercase()
    } else {
        raw.to_lowercase()
    };
    let mut chars = lower.chars();
    let first = chars.next()?;
    if chars.next().is_none() && !(first.is_ascii_digit() || is_cjk(first)) {
        return None;
    }
    if is_stopword(&lower) {
        return None;
    }
    if lower.len() > MAX_NUMBER_BYTES && lower.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if lower.len() >= 3 && lower.bytes().all(|b| b.is_ascii_lowercase()) {
        return Some(porter_stem(&lower));
    }
    Some(lower)
}

/// Lowercase concatenation of an identifier's subwords: the compound term
/// shared by `parse_query`, `parseQuery`, and `ParseQuery`.
fn compound_of(word: &str, subs: &[(usize, usize)]) -> String {
    let mut out = String::with_capacity(word.len());
    for &(a, b) in subs {
        for c in word[a..b].chars() {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn looks_like_hash(word: &str) -> bool {
    word.len() >= MIN_HASH_BYTES && word.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Canonical entity key: lowercase subwords joined by single spaces.
///
/// `parse_query`, `parseQuery`, and "Parse Query" all fold to
/// `parse query`; `Index::search` folds to `index search`; a possessive
/// `Alice's` folds to `alice`. Returns an empty string when nothing
/// alphanumeric remains.
pub(crate) fn fold_key(text: &str) -> String {
    let mut out = String::new();
    let mut subs: Vec<(usize, usize)> = Vec::new();
    for_each_word(text, |word, start| {
        if word == "s" && is_possessive_s(text, start) {
            return;
        }
        split_subwords(word, &mut subs);
        for &(a, b) in &subs {
            if !out.is_empty() {
                out.push(' ');
            }
            for c in word[a..b].chars() {
                out.extend(c.to_lowercase());
            }
        }
    });
    out
}

fn is_possessive_s(text: &str, start: usize) -> bool {
    text[..start].ends_with('\'') || text[..start].ends_with('\u{2019}')
}

/// The index term that mentions of a code entity produce: the compound for
/// a multi-word key, the normalized word otherwise. Matches what
/// [`analyze`] emits for the identifier the key was folded from.
pub(crate) fn entity_term(key: &str) -> Option<String> {
    let words: Vec<&str> = key.split(' ').filter(|w| !w.is_empty()).collect();
    match words.len() {
        0 => None,
        1 => normalize_token(words[0]),
        _ => Some(words.concat()),
    }
}

/// Distinct normalized, non-compound terms of `text` (used for predicate and
/// title matching where positions do not matter).
pub(crate) fn term_set(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = analyze(text)
        .into_iter()
        .filter(|t| !t.compound)
        .map(|t| t.text)
        .collect();
    terms.sort_unstable();
    terms.dedup();
    terms
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
