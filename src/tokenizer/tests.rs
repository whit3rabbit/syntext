use super::*;

#[test]
fn empty_input_returns_empty() {
    assert!(build_all(b"").is_empty());
    assert!(build_covering(b"").is_none());
}

#[test]
fn short_input_below_min_gram_len() {
    // "ab" is 2 bytes, below MIN_GRAM_LEN=3
    assert!(build_covering(b"ab").is_none());
    // build_all on "ab": one boundary-to-boundary span of length 2, below min
    let grams = build_all(b"ab");
    assert!(grams.is_empty());
}

#[test]
fn lowercase_normalization() {
    // "PARSE" and "parse" must produce the same gram hashes
    let upper = build_all(b"PARSE_QUERY");
    let lower = build_all(b"parse_query");
    assert_eq!(
        upper, lower,
        "uppercase and lowercase must produce same grams"
    );
}

#[test]
fn build_covering_inner_rejects_truncated_fragment_edge() {
    assert!(
        build_covering_inner(b"parse_quer").is_none(),
        "truncated regex literal fragments must not rely on synthetic end boundaries"
    );
}

#[test]
fn build_all_indexes_camel_case_identifiers() {
    let grams = build_all(b"LanguageServerId");
    assert!(
        grams.len() >= 2,
        "camel-case identifiers should contribute extra indexed grams"
    );
}

#[test]
fn build_all_skips_redundant_second_pass_without_camel_case() {
    let input = b"parse_query";
    let lower: Vec<u8> = input.iter().map(|b| b.to_ascii_lowercase()).collect();
    let boundaries = boundary_positions(&lower);
    let mut expected = Vec::new();
    append_grams_for_boundaries(&mut expected, &lower, &boundaries);

    assert_eq!(
        build_all(input),
        expected,
        "non-camel inputs should not pay for a duplicate case-aware pass"
    );
}

#[test]
fn covering_hashes_subset_of_all_hashes() {
    // Every gram in build_covering(s) must also appear in build_all(s).
    // This is the core no-false-negative invariant for the self-contained case.
    let input = b"parse_query";
    let all: std::collections::HashSet<u64> = build_all(input).into_iter().collect();
    let covering = build_covering(input).unwrap_or_default();
    for h in covering {
        assert!(
            all.contains(&h),
            "gram from build_covering not found in build_all on same input"
        );
    }
}

#[test]
fn gram_hash_is_deterministic() {
    let h1 = gram_hash(b"hello");
    let h2 = gram_hash(b"hello");
    assert_eq!(h1, h2);
}

#[test]
fn gram_hash_distinct_for_distinct_grams() {
    let h1 = gram_hash(b"parse");
    let h2 = gram_hash(b"query");
    assert_ne!(h1, h2);
}

#[test]
fn all_same_char_does_not_panic() {
    // "aaa" is 3 bytes, all same. Boundary between identical chars:
    // BIGRAM_WEIGHTS['a','a'] is likely low (common pair), so only boundaries
    // at 0 and 3. Span = 3 = MIN_GRAM_LEN, so one gram emitted.
    let grams = build_all(b"aaa");
    // Either 0 or 1 gram depending on weight; must not panic.
    let _ = grams;
}

#[test]
fn single_byte_does_not_panic() {
    let _ = build_all(b"x");
    assert!(build_covering(b"x").is_none());
}

#[test]
fn boundary_positions_lower_matches_standard_for_lowercase_input() {
    let lower = b"parse_query_and_build";
    assert_eq!(boundary_positions(lower), boundary_positions_lower(lower));
    with_boundary_positions_lower(lower, |buffered| {
        assert_eq!(
            boundary_positions_lower(lower),
            buffered,
            "callback variant must produce same boundaries as non-buffered"
        );
    });
}

#[test]
fn buffered_boundary_same_as_lower_on_shrink_path() {
    let large_lower: Vec<u8> = (0u8..=127u8)
        .cycle()
        .take(8192)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    // Warm up the thread-local buffer with a large input to trigger the shrink path.
    with_boundary_positions_lower(&large_lower, |_| {});

    let small_lower: Vec<u8> = b"fn foo".iter().map(|b| b.to_ascii_lowercase()).collect();
    let from_lower = boundary_positions_lower(&small_lower);
    with_boundary_positions_lower(&small_lower, |from_callback| {
        assert_eq!(
            from_lower, from_callback,
            "callback variant must produce same boundaries as non-buffered after shrink"
        );
    });
}

#[test]
fn long_token_without_internal_boundaries() {
    // A 200-byte string of 'a' with no forced or weight-based internal
    // boundaries: only one span [0,200]. That exceeds MAX_GRAM_LEN=128,
    // so no gram is emitted. build_covering also returns None.
    let input = vec![b'a'; 200];
    let grams = build_all(&input);
    assert!(grams.is_empty());
    assert!(build_covering(&input).is_none());
}
