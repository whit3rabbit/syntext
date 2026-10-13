//! Line index over a document's text.

/// Byte offsets of every line start, so line ranges convert to byte ranges
/// in O(1). Lines are 0-based here; passages report 1-based numbers.
pub(crate) struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' && i + 1 < text.len() {
                starts.push(i + 1);
            }
        }
        if text.is_empty() {
            starts.clear();
        }
        Lines { text, starts }
    }

    /// Number of lines (a trailing newline does not start an extra line).
    pub(crate) fn len(&self) -> usize {
        self.starts.len()
    }

    /// Byte offset where line `i` starts; `len()` maps to the text end.
    pub(crate) fn start(&self, i: usize) -> usize {
        self.starts.get(i).copied().unwrap_or(self.text.len())
    }

    /// Text of line `i` without its line terminator.
    pub(crate) fn line(&self, i: usize) -> &'a str {
        let s = self.start(i);
        let e = self.start(i + 1);
        self.text[s..e].trim_end_matches(['\n', '\r'])
    }

    /// True if line `i` is empty or whitespace only.
    pub(crate) fn is_blank(&self, i: usize) -> bool {
        self.line(i).trim().is_empty()
    }

    /// Byte length of lines `[l0, l1)` including terminators.
    pub(crate) fn bytes(&self, l0: usize, l1: usize) -> usize {
        self.start(l1) - self.start(l0)
    }

    /// Line containing byte offset `off` (offsets past the end map to the
    /// last line).
    pub(crate) fn line_of(&self, off: usize) -> usize {
        match self.starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// Shrink `[l0, l1)` so it neither starts nor ends on a blank line.
    /// Returns `None` when every line in the range is blank.
    pub(crate) fn trim_blank(&self, l0: usize, l1: usize) -> Option<(usize, usize)> {
        let mut a = l0;
        let mut b = l1;
        while a < b && self.is_blank(a) {
            a += 1;
        }
        while b > a && self.is_blank(b - 1) {
            b -= 1;
        }
        (a < b).then_some((a, b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_lines_with_and_without_trailing_newline() {
        let l = Lines::new("a\nbb\n");
        assert_eq!(l.len(), 2);
        assert_eq!(l.line(0), "a");
        assert_eq!(l.line(1), "bb");
        assert_eq!(l.start(2), 5);
        let l = Lines::new("a\nbb");
        assert_eq!(l.len(), 2);
        assert_eq!(l.line(1), "bb");
        assert_eq!(Lines::new("").len(), 0);
    }

    #[test]
    fn crlf_terminators_are_stripped() {
        let l = Lines::new("x\r\ny\r\n");
        assert_eq!(l.line(0), "x");
        assert_eq!(l.line(1), "y");
    }

    #[test]
    fn byte_to_line_and_trimming() {
        let l = Lines::new("\n\nab\ncd\n\n");
        assert_eq!(l.line_of(0), 0);
        assert_eq!(l.line_of(2), 2);
        assert_eq!(l.line_of(4), 2);
        assert_eq!(l.line_of(99), l.len() - 1);
        assert_eq!(l.trim_blank(0, l.len()), Some((2, 4)));
        assert_eq!(l.trim_blank(0, 2), None);
        assert_eq!(l.bytes(2, 4), 6);
    }
}
