//! Word scanning and identifier splitting.

/// True for characters of scripts written without spaces between words (CJK
/// ideographs, kana, hangul). Each such character is indexed as its own word.
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3040..=0x30FF       // hiragana, katakana
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified ideographs
            | 0xAC00..=0xD7AF // hangul syllables
            | 0xF900..=0xFAFF // CJK compatibility ideographs
            | 0x20000..=0x2A6DF // CJK extension B
    )
}

/// Call `f(word, byte_start)` for every word in `text`.
///
/// A word is a maximal run of alphanumeric characters and underscores, so
/// `parse_query` and `parseQuery` arrive whole and are split into subwords
/// later. Every CJK character is reported as a one-character word.
pub(crate) fn for_each_word<'a>(text: &'a str, mut f: impl FnMut(&'a str, usize)) {
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if is_cjk(c) {
            if let Some(s) = start.take() {
                f(&text[s..i], s);
            }
            f(&text[i..i + c.len_utf8()], i);
        } else if c.is_alphanumeric() || c == '_' {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            f(&text[s..i], s);
        }
    }
    if let Some(s) = start {
        f(&text[s..], s);
    }
}

/// Split an identifier into subword byte ranges (relative to `word`).
///
/// Boundaries fall at underscores, at lower-to-upper transitions
/// (`parseQuery` -> `parse|Query`), at digit-to-upper transitions
/// (`utf8Decoder` -> `utf8|Decoder`), and before the last capital of an
/// acronym followed by a lowercase run (`HTTPServer` -> `HTTP|Server`). An
/// acronym plural such as `URLs` stays whole. Letter-digit runs (`base64`,
/// `v2`) are not split.
pub(crate) fn split_subwords(word: &str, out: &mut Vec<(usize, usize)>) {
    out.clear();
    let mut part_start = 0;
    for part in word.split('_') {
        let begin = part_start;
        part_start += part.len() + 1;
        if part.is_empty() {
            continue;
        }
        let chars: Vec<(usize, char)> = part.char_indices().collect();
        let mut seg = 0;
        for idx in 1..chars.len() {
            let (pos, cur) = chars[idx];
            let prev = chars[idx - 1].1;
            if !cur.is_uppercase() {
                continue;
            }
            let after_lower = prev.is_lowercase() || prev.is_ascii_digit();
            let acronym_end = prev.is_uppercase()
                && chars
                    .get(idx + 1)
                    .is_some_and(|&(_, next)| next.is_lowercase())
                && !is_plural_tail(&chars, idx + 1);
            if after_lower || acronym_end {
                out.push((begin + seg, begin + pos));
                seg = pos;
            }
        }
        out.push((begin + seg, begin + part.len()));
    }
}

/// True when the lowercase run starting at `idx` is a lone trailing `s`
/// (`URLs`, `IDs`), which pluralizes the acronym rather than starting a word.
fn is_plural_tail(chars: &[(usize, char)], idx: usize) -> bool {
    idx + 1 == chars.len() && chars[idx].1 == 's'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for_each_word(text, |w, _| out.push(w.to_string()));
        out
    }

    fn subwords(word: &str) -> Vec<&str> {
        let mut ranges = Vec::new();
        split_subwords(word, &mut ranges);
        ranges.iter().map(|&(a, b)| &word[a..b]).collect()
    }

    #[test]
    fn scans_identifiers_whole_and_separates_punctuation() {
        assert_eq!(
            words("fn parse_query(args) -> Index::search"),
            ["fn", "parse_query", "args", "Index", "search"]
        );
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words("  x  "), ["x"]);
    }

    #[test]
    fn cjk_characters_are_single_words() {
        assert_eq!(words("検索engine"), ["検", "索", "engine"]);
    }

    #[test]
    fn word_offsets_are_byte_offsets() {
        let mut seen = Vec::new();
        for_each_word("é foo", |w, s| seen.push((w.to_string(), s)));
        assert_eq!(seen, [("é".to_string(), 0), ("foo".to_string(), 3)]);
    }

    #[test]
    fn splits_snake_and_camel_case() {
        assert_eq!(subwords("parse_query"), ["parse", "query"]);
        assert_eq!(subwords("parseQuery"), ["parse", "Query"]);
        assert_eq!(subwords("ParseQuery"), ["Parse", "Query"]);
        assert_eq!(subwords("HTTPServer"), ["HTTP", "Server"]);
        assert_eq!(subwords("XMLHttpRequest"), ["XML", "Http", "Request"]);
        assert_eq!(subwords("utf8Decoder"), ["utf8", "Decoder"]);
        assert_eq!(subwords("__init__"), ["init"]);
        assert_eq!(subwords("MAX_LEN"), ["MAX", "LEN"]);
    }

    #[test]
    fn keeps_acronym_plurals_and_digit_runs() {
        assert_eq!(subwords("URLs"), ["URLs"]);
        assert_eq!(subwords("userIDs"), ["user", "IDs"]);
        assert_eq!(subwords("base64"), ["base64"]);
        assert_eq!(subwords("v2"), ["v2"]);
    }

    #[test]
    fn underscores_only_yield_nothing() {
        assert!(subwords("___").is_empty());
    }
}
