//! Encoding normalization: BOM stripping and UTF-16 transcoding.
//!
//! Apply `normalize_encoding` on raw file bytes BEFORE calling `is_binary()`.
//! UTF-16 LE/BE files have embedded null bytes that would otherwise trigger
//! the binary-skip heuristic; transcoding to UTF-8 removes those nulls and
//! makes the content indexable.

use std::borrow::Cow;

/// Normalize raw file bytes for indexing and search.
///
/// - Strips the UTF-8 BOM (`EF BB BF`) and returns the remainder.
/// - Detects UTF-16 LE BOM (`FF FE`) and transcodes to UTF-8.
/// - Detects UTF-16 BE BOM (`FE FF`) and transcodes to UTF-8.
/// - Returns all other content as `Cow::Borrowed` (zero copy).
///
/// **Must be called before `is_binary()`**: UTF-16 files have null bytes at
/// every other character position and would be silently skipped without this.
///
/// When `verbose` is true, emits a warning to stderr if a UTF-16 file has an
/// odd byte count after the BOM (truncated on disk); the trailing byte is
/// silently dropped by `chunks_exact(2)`, which can cause false negatives.
pub(crate) fn normalize_encoding(content: &[u8], verbose: bool) -> Cow<'_, [u8]> {
    if let Some(rest) = content.strip_prefix(b"\xEF\xBB\xBF") {
        return Cow::Borrowed(rest);
    }
    if let Some(rest) = content.strip_prefix(b"\xFF\xFE") {
        return Cow::Owned(decode_utf16(rest, u16::from_le_bytes, verbose));
    }
    if let Some(rest) = content.strip_prefix(b"\xFE\xFF") {
        return Cow::Owned(decode_utf16(rest, u16::from_be_bytes, verbose));
    }
    Cow::Borrowed(content)
}

fn decode_utf16(bytes: &[u8], from_bytes: fn([u8; 2]) -> u16, verbose: bool) -> Vec<u8> {
    let chunks = bytes.chunks_exact(2);
    if verbose && !chunks.remainder().is_empty() {
        eprintln!(
            "syntext: warning: UTF-16 file has odd byte count ({} bytes after BOM); trailing byte dropped",
            bytes.len()
        );
    }
    char::decode_utf16(chunks.map(|c| from_bytes([c[0], c[1]])))
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_bom_returns_borrowed() {
        let content = b"fn main() {}";
        let result = normalize_encoding(content, false);
        assert!(
            matches!(result, Cow::Borrowed(_)),
            "plain UTF-8 must return Cow::Borrowed (zero copy)"
        );
        assert_eq!(result.as_ref(), content);
    }

    #[test]
    fn utf8_bom_stripped() {
        let input = b"\xEF\xBB\xBFfn main() {}";
        let result = normalize_encoding(input, false);
        assert_eq!(result.as_ref(), b"fn main() {}");
    }

    #[test]
    fn utf8_bom_only_file() {
        let result = normalize_encoding(b"\xEF\xBB\xBF", false);
        assert_eq!(result.as_ref(), b"");
    }

    #[test]
    fn utf16_le_ascii_transcoded() {
        // "hi\n" in UTF-16 LE with BOM: FF FE 68 00 69 00 0A 00
        let input: &[u8] = b"\xFF\xFEh\x00i\x00\n\x00";
        let result = normalize_encoding(input, false);
        assert_eq!(result.as_ref(), b"hi\n");
    }

    #[test]
    fn utf16_be_ascii_transcoded() {
        // "hi\n" in UTF-16 BE with BOM: FE FF 00 68 00 69 00 0A
        let input: &[u8] = b"\xFE\xFF\x00h\x00i\x00\n";
        let result = normalize_encoding(input, false);
        assert_eq!(result.as_ref(), b"hi\n");
    }

    #[test]
    fn utf16_le_non_bmp_replacement_char() {
        // Lone high surrogate (D800) -> U+FFFD (EF BF BD in UTF-8)
        let input: &[u8] = b"\xFF\xFE\x00\xD8"; // BOM + lone surrogate
        let result = normalize_encoding(input, false);
        assert_eq!(result.as_ref(), "\u{FFFD}".as_bytes());
    }

    #[test]
    fn utf16_le_odd_byte_trailing_truncated() {
        // Odd byte after BOM dropped by chunks_exact(2)
        let input: &[u8] = b"\xFF\xFEh\x00i"; // BOM + "h" + lone byte
        let result = normalize_encoding(input, false);
        assert_eq!(result.as_ref(), b"h");
    }

    #[test]
    fn empty_content_returns_borrowed() {
        let result = normalize_encoding(b"", false);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result.as_ref(), b"");
    }

    #[test]
    fn utf16_le_source_code() {
        let src = "fn main() {}";
        let utf16le: Vec<u8> = src.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut input = vec![0xFF, 0xFE]; // LE BOM
        input.extend_from_slice(&utf16le);
        let result = normalize_encoding(&input, false);
        assert_eq!(result.as_ref(), src.as_bytes());
    }

    #[test]
    fn utf16_le_odd_byte_verbose_warning() {
        // verbose=true should not panic; warning goes to stderr (not testable here,
        // but we verify the output is still correct).
        let input: &[u8] = b"\xFF\xFEh\x00i"; // BOM + "h" + lone byte
        let result = normalize_encoding(input, true);
        assert_eq!(result.as_ref(), b"h");
    }
}
