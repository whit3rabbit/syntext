use super::porter_stem;

/// Pairs from the reference vocabulary (`voc.txt` / `output.txt`) and the
/// paper's worked examples.
const VECTORS: &[(&str, &str)] = &[
    ("caresses", "caress"),
    ("ponies", "poni"),
    ("ties", "ti"),
    ("caress", "caress"),
    ("cats", "cat"),
    ("feed", "feed"),
    ("agreed", "agre"),
    ("plastered", "plaster"),
    ("bled", "bled"),
    ("motoring", "motor"),
    ("sing", "sing"),
    ("conflated", "conflat"),
    ("troubled", "troubl"),
    ("sized", "size"),
    ("hopping", "hop"),
    ("tanned", "tan"),
    ("falling", "fall"),
    ("hissing", "hiss"),
    ("fizzed", "fizz"),
    ("failing", "fail"),
    ("filing", "file"),
    ("happy", "happi"),
    ("sky", "sky"),
    ("relational", "relat"),
    ("conditional", "condit"),
    ("rational", "ration"),
    ("valenci", "valenc"),
    ("hesitanci", "hesit"),
    ("digitizer", "digit"),
    ("conformabli", "conform"),
    ("radicalli", "radic"),
    ("differentli", "differ"),
    ("vileli", "vile"),
    ("analogousli", "analog"),
    ("vietnamization", "vietnam"),
    ("predication", "predic"),
    ("operator", "oper"),
    ("feudalism", "feudal"),
    ("decisiveness", "decis"),
    ("hopefulness", "hope"),
    ("callousness", "callous"),
    ("formaliti", "formal"),
    ("sensitiviti", "sensit"),
    ("sensibiliti", "sensibl"),
    ("triplicate", "triplic"),
    ("formative", "form"),
    ("formalize", "formal"),
    ("electriciti", "electr"),
    ("electrical", "electr"),
    ("hopeful", "hope"),
    ("goodness", "good"),
    ("revival", "reviv"),
    ("allowance", "allow"),
    ("inference", "infer"),
    ("airliner", "airlin"),
    ("gyroscopic", "gyroscop"),
    ("adjustable", "adjust"),
    ("defensible", "defens"),
    ("irritant", "irrit"),
    ("replacement", "replac"),
    ("adjustment", "adjust"),
    ("dependent", "depend"),
    ("adoption", "adopt"),
    ("homologou", "homolog"),
    ("communism", "commun"),
    ("activate", "activ"),
    ("angulariti", "angular"),
    ("homologous", "homolog"),
    ("effective", "effect"),
    ("bowdlerize", "bowdler"),
    ("probate", "probat"),
    ("rate", "rate"),
    ("cease", "ceas"),
    ("controll", "control"),
    ("roll", "roll"),
    ("generalizations", "gener"),
    ("oscillators", "oscil"),
    ("abandoned", "abandon"),
    ("abilities", "abil"),
    ("abbey", "abbei"),
    ("absolutely", "absolut"),
];

#[test]
fn reference_vectors() {
    for (input, expected) in VECTORS {
        assert_eq!(porter_stem(input), *expected, "stem({input})");
    }
}

#[test]
fn inflections_collapse_to_one_stem() {
    let forms = [
        "validate",
        "validates",
        "validated",
        "validating",
        "validation",
    ];
    for form in forms {
        assert_eq!(porter_stem(form), "valid", "{form}");
    }
    assert_eq!(porter_stem("queries"), porter_stem("query"));
    assert_eq!(porter_stem("parsing"), porter_stem("parse"));
    assert_eq!(porter_stem("credentials"), porter_stem("credential"));
}

#[test]
fn short_and_non_alphabetic_words_pass_through() {
    for word in ["a", "is", "s", "utf8", "x86", "e2e", "Mixed", "naïve"] {
        assert_eq!(porter_stem(word), word);
    }
}

#[test]
fn degenerate_words_terminate() {
    // Whole word is a suffix; the stem end index becomes -1.
    for word in [
        "ies", "ing", "eed", "sss", "ion", "ness", "ful", "yyy", "eee",
    ] {
        let _ = porter_stem(word);
    }
    assert_eq!(porter_stem("ies"), "i");
    assert_eq!(porter_stem("string"), "string");
    assert_eq!(porter_stem("thing"), "thing");
}
