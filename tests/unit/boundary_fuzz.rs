//! Property-based tests for the forced boundary coverage invariant.
//!
//! The core property: for any document D and any token-aligned substring Q
//! (bounded by forced boundary characters), every gram in build_covering(Q)
//! must appear in build_all(D). This guarantees zero false negatives for
//! the gram index when queries are token-aligned.

use std::collections::HashSet;

use proptest::prelude::*;
use ripline::tokenizer::{build_all, build_covering, MIN_GRAM_LEN};

// ---------------------------------------------------------------------------
// Curated invariant test (deterministic, exhaustive on small inputs)
// ---------------------------------------------------------------------------

/// For curated documents, verify the covering invariant holds for every
/// token-aligned substring. Token-aligned means the substring starts and
/// ends at positions where forced boundary characters appear.
#[test]
fn covering_subset_of_all_in_context() {
    let documents: &[&[u8]] = &[
        b"fn parse_query(args: &str) -> Query {",
        b"def process_batch(items, config):",
        b"import { HashMap } from 'collections';",
        b"let result = self.name.to_string();",
        b"TODO: fix this before release",
        b"user@example.com sent 192.168.1.1",
        b"camelCaseIdentifier = getValue()",
        b"__init__(self, parse_query_engine)",
        b"PARSE_QUERY_MAX_LEN = 4096",
        b"  for item in items:\n    process_batch(item)",
    ];

    for doc in documents {
        let all: HashSet<u64> = build_all(doc).into_iter().collect();

        // Extract forced-boundary tokens from the document and verify each
        // one's covering grams are in build_all(doc).
        let lower: Vec<u8> = doc.iter().map(|b| b.to_ascii_lowercase()).collect();
        let boundaries = forced_boundary_positions(&lower);

        for i in 0..boundaries.len() {
            for j in (i + 1)..boundaries.len() {
                let start = boundaries[i];
                let end = boundaries[j];
                if end - start < MIN_GRAM_LEN {
                    continue;
                }

                let substr = &lower[start..end];
                if let Some(covering) = build_covering(substr) {
                    for h in &covering {
                        assert!(
                            all.contains(h),
                            "VIOLATION: query={:?} in doc={:?}, gram {:016x} not found\n\
                             boundaries: {:?}\nquery span: [{}..{}]",
                            String::from_utf8_lossy(substr),
                            String::from_utf8_lossy(doc),
                            h,
                            &boundaries[i..=j],
                            start,
                            end
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property-based: token-aligned invariant (must have ZERO failures)
// ---------------------------------------------------------------------------

/// Generate strings that look like code: identifiers separated by
/// punctuation and whitespace.
fn code_like_string() -> impl Strategy<Value = Vec<u8>> {
    let token = "[a-z][a-z0-9]{2,12}";
    let separator = prop_oneof![
        Just(" ".to_string()),
        Just("(".to_string()),
        Just(")".to_string()),
        Just(".".to_string()),
        Just("_".to_string()),
        Just(", ".to_string()),
        Just(": ".to_string()),
        Just("\n".to_string()),
        Just(" = ".to_string()),
        Just(";".to_string()),
    ];

    prop::collection::vec((token, separator), 3..15).prop_map(|pairs| {
        let mut s = String::new();
        for (tok, sep) in pairs {
            s.push_str(&tok);
            s.push_str(&sep);
        }
        s.into_bytes()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5_000))]

    /// For any generated code-like document, the token-aligned covering
    /// invariant must hold: every gram in build_covering(token) must
    /// appear in build_all(document).
    #[test]
    fn token_aligned_invariant(doc in code_like_string()) {
        let lower: Vec<u8> = doc.iter().map(|b| b.to_ascii_lowercase()).collect();
        let all: HashSet<u64> = build_all(&doc).into_iter().collect();
        let boundaries = forced_boundary_positions(&lower);

        for i in 0..boundaries.len() {
            for j in (i + 1)..boundaries.len() {
                let start = boundaries[i];
                let end = boundaries[j];
                if end - start < MIN_GRAM_LEN {
                    continue;
                }

                let substr = &lower[start..end];
                if let Some(covering) = build_covering(substr) {
                    for h in &covering {
                        prop_assert!(
                            all.contains(h),
                            "VIOLATION: query={:?} in doc context, gram not found",
                            String::from_utf8_lossy(substr)
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Property-based: arbitrary substring coverage (informational)
// ---------------------------------------------------------------------------

/// Quantifies how often non-token-aligned substrings cause coverage
/// violations. This test does NOT assert; it logs violation rates.
/// If the violation rate exceeds 5%, overlapping trigrams (Phase B)
/// are needed to cover mid-token substring queries.
///
/// Run with: cargo test --test boundary_fuzz arbitrary_substring -- --nocapture
#[test]
fn arbitrary_substring_coverage() {
    use proptest::test_runner::{Config, TestRunner};

    let config = Config::with_cases(5_000);
    let mut runner = TestRunner::new(config);

    use std::cell::Cell;
    let total_queries = Cell::new(0u64);
    let total_violations = Cell::new(0u64);
    let total_grams_checked = Cell::new(0u64);

    let result = runner.run(
        &(code_like_string(), 0.0f64..1.0, 0.01f64..0.3),
        |(doc, start_frac, len_frac)| {
            if doc.len() < MIN_GRAM_LEN {
                return Ok(());
            }
            let start = (start_frac * doc.len() as f64) as usize;
            let len = ((len_frac * doc.len() as f64) as usize).max(MIN_GRAM_LEN);
            let end = (start + len).min(doc.len());
            let start = start.min(end.saturating_sub(MIN_GRAM_LEN));
            if end - start < MIN_GRAM_LEN {
                return Ok(());
            }

            let lower: Vec<u8> = doc.iter().map(|b| b.to_ascii_lowercase()).collect();
            let all: std::collections::HashSet<u64> = build_all(&doc).into_iter().collect();
            let substr = &lower[start..end];

            if let Some(covering) = build_covering(substr) {
                total_queries.set(total_queries.get() + 1);
                total_grams_checked.set(total_grams_checked.get() + covering.len() as u64);
                let violations = covering.iter().filter(|h| !all.contains(h)).count();
                if violations > 0 {
                    total_violations.set(total_violations.get() + 1);
                }
            }

            Ok(())
        },
    );

    assert!(result.is_ok(), "proptest runner failed: {:?}", result);

    let tq = total_queries.get();
    let tv = total_violations.get();
    let tg = total_grams_checked.get();
    let violation_rate = if tq > 0 {
        tv as f64 / tq as f64 * 100.0
    } else {
        0.0
    };

    eprintln!(
        "\n--- Non-aligned substring coverage report ---\n\
         Total queries with grams: {}\n\
         Queries with violations:  {} ({:.2}%)\n\
         Total grams checked:      {}\n\
         Verdict: {}\n",
        tq,
        tv,
        violation_rate,
        tg,
        if violation_rate < 5.0 {
            "OK -- overlapping trigrams not needed"
        } else {
            "HIGH -- overlapping trigrams recommended"
        }
    );
}

// ---------------------------------------------------------------------------
// Helper: extract forced boundary positions (mirrors tokenizer logic)
// ---------------------------------------------------------------------------

fn is_forced_boundary(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C
            | b'(' | b')' | b'{' | b'}' | b'[' | b']' | b'<' | b'>'
            | b'.' | b',' | b':' | b';'
            | b'=' | b'+' | b'-' | b'*' | b'/' | b'%'
            | b'!' | b'&' | b'|' | b'^' | b'~'
            | b'"' | b'\'' | b'`'
            | b'@' | b'#' | b'$' | b'?'
            | b'_'
            | 0x00..=0x08 | 0x0E..=0x1F | 0x7F
    )
}

fn forced_boundary_positions(bytes: &[u8]) -> Vec<usize> {
    let mut positions = vec![0];
    for i in 1..bytes.len() {
        if is_forced_boundary(bytes[i]) || is_forced_boundary(bytes[i - 1]) {
            positions.push(i);
        }
    }
    if !bytes.is_empty() {
        positions.push(bytes.len());
    }
    positions.dedup();
    positions
}
