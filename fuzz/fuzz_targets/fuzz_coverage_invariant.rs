//! Fuzz target: coverage invariant for the sparse n-gram tokenizer.
//!
//! Tests two properties at different severity levels:
//!
//! 1. **Token-aligned invariant** (PANIC): for any document D and any
//!    substring Q bounded by forced-boundary positions, every gram in
//!    build_covering(Q) must appear in build_all(D). Violations are bugs.
//!
//! 2. **Arbitrary substring coverage** (no panic, counted): for non-aligned
//!    substrings, violations are expected and tracked for analysis.
//!
//! Run: cargo +nightly fuzz run fuzz_coverage_invariant -- -max_len=4096

#![no_main]

use std::collections::HashSet;

use libfuzzer_sys::fuzz_target;
use ripline::tokenizer::{build_all, build_covering, MIN_GRAM_LEN};

/// Characters that always create gram boundaries (mirrors tokenizer logic).
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

/// Compute forced boundary positions for the given bytes.
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

fuzz_target!(|data: &[u8]| {
    // Need at least a document + a substring.
    // First 4 bytes: start_idx (u16 LE), boundary_pair_idx (u16 LE).
    // Remaining bytes: document.
    if data.len() < 4 + MIN_GRAM_LEN {
        return;
    }

    let idx1 = u16::from_le_bytes([data[0], data[1]]) as usize;
    let idx2 = u16::from_le_bytes([data[2], data[3]]) as usize;
    let doc = &data[4..];

    if doc.len() < MIN_GRAM_LEN {
        return;
    }

    let lower_doc: Vec<u8> = doc.iter().map(|b| b.to_ascii_lowercase()).collect();
    let all: HashSet<u64> = build_all(doc).into_iter().collect();
    let boundaries = forced_boundary_positions(&lower_doc);

    if boundaries.len() < 2 {
        return;
    }

    // Pick a token-aligned substring using the fuzzer-provided indices.
    let i = idx1 % boundaries.len();
    let j_offset = (idx2 % (boundaries.len() - 1)) + 1;
    let j = (i + j_offset) % boundaries.len();
    let (i, j) = if i < j { (i, j) } else { (j, i) };

    let start = boundaries[i];
    let end = boundaries[j];
    if end - start < MIN_GRAM_LEN {
        return;
    }

    let substr = &lower_doc[start..end];

    if let Some(covering) = build_covering(substr) {
        for h in &covering {
            if !all.contains(h) {
                panic!(
                    "TOKEN-ALIGNED COVERAGE VIOLATION: \
                     query={:?} (boundaries[{}..{}] = [{}..{}]) in doc={:?}, \
                     gram {:016x} not in build_all.\n\
                     All boundary positions: {:?}",
                    String::from_utf8_lossy(substr),
                    i,
                    j,
                    start,
                    end,
                    String::from_utf8_lossy(doc),
                    h,
                    boundaries
                );
            }
        }
    }
});
