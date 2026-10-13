//! Relation classes: stems that name the same relation.
//!
//! A question rarely uses the verb its evidence uses: "Who *founded* X?" is
//! answered by "Y *started* X", and "X's *employer*" by "Z *employs* X". The
//! graph walk and chain lifting compare a relation's predicate with the
//! question through these classes, so the right edge is preferred without a
//! model. Small and English-only on purpose; anything broader is the job of
//! a semantic signal, not a word list.

/// Porter stems, each with its class. Sorted by stem.
const CLASSES: &[(&str, &str)] = &[
    ("acquir", "acquire"),
    ("acquisit", "acquire"),
    ("author", "write"),
    ("base", "locate"),
    ("bought", "acquire"),
    ("bui", "acquire"),
    ("build", "make"),
    ("built", "make"),
    ("ceo", "lead"),
    ("cofound", "found"),
    ("creat", "found"),
    ("design", "make"),
    ("develop", "make"),
    ("direct", "lead"),
    ("emploi", "employ"),
    ("employ", "employ"),
    ("employe", "employ"),
    ("establish", "found"),
    ("found", "found"),
    ("founder", "found"),
    ("head", "lead"),
    ("headquart", "locate"),
    ("hire", "employ"),
    ("join", "employ"),
    ("launch", "found"),
    ("lead", "lead"),
    ("led", "lead"),
    ("locat", "locate"),
    ("made", "make"),
    ("make", "make"),
    ("maker", "make"),
    ("manag", "lead"),
    ("manufactur", "make"),
    ("offic", "locate"),
    ("own", "own"),
    ("owner", "own"),
    ("parent", "own"),
    ("produc", "make"),
    ("purchas", "acquire"),
    ("run", "lead"),
    ("sell", "make"),
    ("start", "found"),
    ("subsidiari", "own"),
    ("work", "employ"),
    ("write", "write"),
    ("wrote", "write"),
];

/// The relation class of a Porter stem, if it names one.
pub(crate) fn relation_class(stem: &str) -> Option<&'static str> {
    CLASSES
        .binary_search_by(|(s, _)| (*s).cmp(stem))
        .ok()
        .map(|i| CLASSES[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::porter_stem;

    #[test]
    fn classes_are_sorted_and_reached_through_the_stemmer() {
        assert!(CLASSES.windows(2).all(|w| w[0].0 < w[1].0));
        for (word, class) in [
            ("founded", "found"),
            ("started", "found"),
            ("employs", "employ"),
            ("employer", "employ"),
            ("acquired", "acquire"),
            ("bought", "acquire"),
            ("buys", "acquire"),
            ("headquartered", "locate"),
            ("makes", "make"),
            ("maker", "make"),
        ] {
            assert_eq!(relation_class(&porter_stem(word)), Some(class), "{word}");
        }
        assert_eq!(relation_class("queue"), None);
    }
}
