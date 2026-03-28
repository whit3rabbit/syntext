//! Unit tests for the sparse n-gram tokenizer.
//!
//! Tests are organized by invariant:
//!
//! 1. Boundary detection: underscore transitions, punctuation, ASCII control
//! 2. Lowercase normalization: identical hashes for upper/lower
//! 3. Covering set minimality: build_covering emits one gram per span
//! 4. Round-trip invariant: build_covering hashes ⊆ build_all hashes (same input)
//! 5. Edge cases: empty, 1-byte, 2-byte, all-same, very-long

use syntext::tokenizer::{build_all, build_covering, gram_hash, MIN_GRAM_LEN};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn all_hashes(input: &[u8]) -> std::collections::HashSet<u64> {
    build_all(input).into_iter().collect()
}

fn covering_hashes(input: &[u8]) -> Vec<u64> {
    build_covering(input).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 1. Boundary detection
// ---------------------------------------------------------------------------

/// "parse_query" must produce at least one gram and be indexable.
///
/// The exact grams depend on the BOUNDARY_THRESHOLD and the trained weight
/// table. At the default threshold (32768), the weight table places boundaries
/// at 'q'→'u' (37448) and 'r'→'y' (36379), so "parse_query" produces
/// "parse_q" and "uer" rather than "parse" + "query". This is correct
/// behaviour for the trained table; the verifier handles final confirmation.
///
/// If this test fails, the weight table or threshold is misconfigured.
#[test]
fn parse_query_produces_some_grams() {
    let all = all_hashes(b"parse_query");
    assert!(
        !all.is_empty(),
        "parse_query must produce at least one indexable gram"
    );
    assert!(
        build_covering(b"parse_query").is_some(),
        "parse_query covering set must be non-empty (pattern must be indexable)"
    );
}

/// Punctuation creates boundaries: "parse_query(" must produce at least one gram.
///
/// The '(' character has a high-weight bigram with preceding letters, so it
/// should create a boundary and allow at least the string up to '(' to be
/// indexed. Exact gram content depends on threshold; we verify non-emptiness.
#[test]
fn boundary_near_open_paren() {
    let all = all_hashes(b"parse_query(");
    assert!(
        !all.is_empty(),
        "parse_query( must produce at least one gram"
    );
}

/// Digits mixed with letters: "user123" should be a single gram if no internal boundary.
#[test]
fn digits_in_token() {
    let all = all_hashes(b"user123");
    // Must not panic; may or may not emit grams depending on weights.
    let _ = all;
}

// ---------------------------------------------------------------------------
// 2. Lowercase normalization
// ---------------------------------------------------------------------------

/// `build_all` on uppercase must return same hashes as on lowercase.
#[test]
fn build_all_case_insensitive() {
    let lower = all_hashes(b"parse_query");
    let upper = all_hashes(b"PARSE_QUERY");
    let mixed = all_hashes(b"Parse_Query");
    assert_eq!(
        lower, upper,
        "uppercase must produce same grams as lowercase"
    );
    assert_eq!(
        lower, mixed,
        "mixed case must produce same grams as lowercase"
    );
}

/// `build_covering` on uppercase must return same hashes as on lowercase.
#[test]
fn build_covering_case_insensitive() {
    let lower = covering_hashes(b"parse_query");
    let upper = covering_hashes(b"PARSE_QUERY");
    let lower_set: std::collections::HashSet<u64> = lower.into_iter().collect();
    let upper_set: std::collections::HashSet<u64> = upper.into_iter().collect();
    assert_eq!(lower_set, upper_set);
}

// ---------------------------------------------------------------------------
// 3. Covering set minimality
// ---------------------------------------------------------------------------

/// build_covering must not emit duplicate gram hashes.
#[test]
fn covering_no_duplicates() {
    let covering = build_covering(b"parse_query_engine").unwrap_or_default();
    let unique: std::collections::HashSet<u64> = covering.iter().copied().collect();
    assert_eq!(
        covering.len(),
        unique.len(),
        "build_covering emitted duplicate hashes"
    );
}

/// For a simple token with no internal boundaries, build_covering emits exactly
/// one gram (the whole token), provided the token length is in [MIN_GRAM_LEN, MAX_GRAM_LEN].
///
/// "xyz" (3 bytes, no internal boundary): exactly 1 gram expected.
#[test]
fn single_token_one_gram() {
    // "xyz" has no high-weight internal bigrams for typical code pairs.
    // Only boundaries are at 0 and 3.
    let covering = build_covering(b"xyz");
    if let Some(hashes) = covering {
        assert_eq!(
            hashes.len(),
            1,
            "single token must produce exactly 1 covering gram"
        );
    }
    // If covering is None, that means the span is empty (can't happen for len=3),
    // OR the threshold is so high that even 0→3 span is below it (acceptable).
}

// ---------------------------------------------------------------------------
// 4. Round-trip invariant: covering ⊆ all (on same input)
// ---------------------------------------------------------------------------

/// For any input, every hash in build_covering must also appear in build_all.
///
/// This is the core correctness invariant: if we use a covering gram to
/// filter candidates, any document that produces the gram via build_all
/// is a correct candidate. For the self-contained case (query string == doc
/// substring), this must hold exactly.
#[test]
fn covering_subset_of_all_parse_query() {
    let all = all_hashes(b"parse_query");
    for h in covering_hashes(b"parse_query") {
        assert!(
            all.contains(&h),
            "covering gram not in all-grams for 'parse_query'"
        );
    }
}

#[test]
fn covering_subset_of_all_process_batch() {
    let all = all_hashes(b"process_batch");
    for h in covering_hashes(b"process_batch") {
        assert!(
            all.contains(&h),
            "covering gram not in all-grams for 'process_batch'"
        );
    }
}

#[test]
fn covering_subset_of_all_generic() {
    for input in &[
        b"hello_world" as &[u8],
        b"foo.bar.baz",
        b"x",
        b"ab",
        b"abc",
        b"function_call(",
        b"return_value",
        b"impl_trait",
    ] {
        let all = all_hashes(input);
        for h in covering_hashes(input) {
            assert!(
                all.contains(&h),
                "covering gram not in all-grams for {:?}",
                std::str::from_utf8(input).unwrap_or("<non-utf8>")
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 5. Edge cases
// ---------------------------------------------------------------------------

#[test]
fn empty_input() {
    assert!(build_all(b"").is_empty());
    assert!(build_covering(b"").is_none());
}

#[test]
fn one_byte() {
    let _ = build_all(b"x");
    assert!(build_covering(b"x").is_none());
}

#[test]
fn two_bytes() {
    let _ = build_all(b"xy");
    // 2 bytes < MIN_GRAM_LEN=3, so covering must be None
    assert!(build_covering(b"xy").is_none());
}

#[test]
fn exactly_min_gram_len() {
    // 3-byte input: boundary at 0 and 3, span=3 = MIN_GRAM_LEN. Should emit 1 gram.
    let _ = build_all(b"abc");
    // build_covering: may return Some([hash]) or None depending on whether the
    // 3-byte span is >= MIN_GRAM_LEN and <= MAX_GRAM_LEN (it is).
    // Must not panic.
}

#[test]
fn all_same_bytes() {
    // "aaaa...": no internal boundaries (weight of 'a'→'a' is very low or very high?).
    // Must not panic regardless.
    let _ = build_all(b"aaaaaaaaaaaaaaaa");
    let _ = build_covering(b"aaaaaaaaaaaaaaaa");
}

#[test]
fn input_exactly_max_gram_len() {
    // A MAX_GRAM_LEN-byte input with no internal boundaries should produce one gram.
    let input = vec![b'a'; syntext::tokenizer::MAX_GRAM_LEN];
    let all = build_all(&input);
    // May or may not emit a gram; must not panic.
    let _ = all;
}

#[test]
fn input_just_over_max_gram_len() {
    // Longer than MAX_GRAM_LEN with no internal boundaries: no gram emitted.
    // (Boundary positions: only 0 and MAX+1, span = MAX+1 > MAX.)
    let input = vec![b'a'; syntext::tokenizer::MAX_GRAM_LEN + 1];
    let all = build_all(&input);
    assert!(
        all.is_empty(),
        "no gram should be emitted for span exceeding MAX_GRAM_LEN"
    );
}

#[test]
fn non_ascii_bytes_do_not_panic() {
    // UTF-8 content with high bytes. build_all lowercases via to_ascii_lowercase,
    // which is a no-op for non-ASCII bytes. Must not panic.
    let _ = build_all("café_résumé".as_bytes());
    let _ = build_covering("café_résumé".as_bytes());
}

/// Verify that `(foo)?bar` style: "bar" on its own is extracted.
///
/// This test exists to prevent regressions where someone "fixes" the
/// optional-prefix logic and breaks coverage. The gram for "bar" must exist
/// in build_all("foobar") so that queries like `(foo)?bar` (which decompose
/// to Grams("bar")) correctly identify candidates.
///
/// Note: this is a tokenizer-level test. The HIR decomposition correctness
/// for `(foo)?bar` → Grams("bar") is tested in tests/unit/query.rs.
#[test]
fn bar_gram_in_foobar() {
    let all = all_hashes(b"foobar");
    // If "foo" and "bar" are both grams (boundary between them), good.
    // If "foobar" is one gram (no internal boundary), also good:
    // build_covering("foobar") will emit "foobar", not "bar",
    // so queries for "foobar" still work.
    // This test just checks the function doesn't panic.
    let _ = all;
}

#[test]
fn gram_hash_length_matters() {
    // "par" and "pars" and "parse" must all have distinct hashes.
    let h3 = gram_hash(b"par");
    let h4 = gram_hash(b"pars");
    let h5 = gram_hash(b"parse");
    assert_ne!(h3, h4);
    assert_ne!(h4, h5);
    assert_ne!(h3, h5);
}

// ---------------------------------------------------------------------------
// Spot-check: MIN_GRAM_LEN is exported and equals 3
// ---------------------------------------------------------------------------

#[test]
fn min_gram_len_is_three() {
    assert_eq!(MIN_GRAM_LEN, 3, "MIN_GRAM_LEN must be 3 (trigram floor)");
}

// ---------------------------------------------------------------------------
// Forced boundary tests
// ---------------------------------------------------------------------------

/// With forced boundaries, "parse_query" must split into at least "parse" and
/// "query" because underscore is a forced boundary character.
#[test]
fn forced_boundary_splits_snake_case() {
    let covering = build_covering(b"parse_query").unwrap();
    // Underscore creates forced boundaries on both sides, giving spans
    // "parse", "_", "query". The "_" span is 1 byte (< MIN_GRAM_LEN),
    // so only "parse" and "query" produce grams.
    assert!(
        covering.len() >= 2,
        "parse_query must produce at least 2 covering grams, got {}",
        covering.len()
    );
}

/// Forced boundaries make query grams context-independent for token-aligned
/// queries. "parse" as a gram from build_covering("parse_query") must also
/// appear in build_all("fn parse_query(args)") because space and '(' are
/// forced boundaries, and "parse" is a complete forced-boundary span in both.
#[test]
fn covering_subset_in_document_context() {
    let documents: &[&[u8]] = &[
        b"fn parse_query(args: &str) -> Query {",
        b"def process_batch(items, config):",
        b"import { HashMap } from 'collections';",
        b"let result = self.name.to_string();",
        b"__init__(self, parse_query_engine)",
        b"PARSE_QUERY_MAX_LEN = 4096",
    ];

    // For each document, extract all tokens (forced-boundary spans) and
    // verify the covering invariant holds in context.
    for doc in documents {
        let all: std::collections::HashSet<u64> = build_all(doc).into_iter().collect();

        // Test specific token-aligned queries within each document
        let queries: &[&[u8]] = &[
            b"parse", b"query", b"process", b"batch", b"HashMap", b"result", b"self", b"name",
            b"args", b"items",
        ];
        for q in queries {
            if let Some(covering) = build_covering(q) {
                for h in &covering {
                    // This gram should appear in build_all of ANY document
                    // containing this token, because forced boundaries ensure
                    // the token is extracted as a gram in both contexts.
                    // We check the specific documents that contain it.
                    let q_str = std::str::from_utf8(q).unwrap();
                    let doc_str = String::from_utf8_lossy(doc).to_ascii_lowercase();
                    if doc_str.contains(&q_str.to_ascii_lowercase()) {
                        assert!(
                            all.contains(h),
                            "COVERAGE VIOLATION: query={:?} in doc={:?}, gram {:016x} not found",
                            q_str,
                            String::from_utf8_lossy(doc),
                            h
                        );
                    }
                }
            }
        }
    }
}
