//! Structure-aware condensation of a span that does not fit whole.
//!
//! Instead of distilling a passage to its single best sentence (which drops
//! the "unless", the negation, or the date in the next sentence), the
//! condenser keeps whole lines: the passage's anchor (heading or definition
//! signature), its first line, and windows of context around every line
//! that matched the question. Each omitted stretch becomes an explicit
//! `[... lines A-B omitted ...]` marker, so the model knows text is missing
//! and where. The window shrinks until the result fits.

use crate::tokens::TokenCounter;

pub(crate) struct Condensed {
    pub text: String,
}

/// The token budget a condensed span must fit.
pub(crate) struct Fit<'a> {
    pub counter: &'a dyn TokenCounter,
    /// Tokens available for the whole block.
    pub max_tokens: usize,
    /// Tokens of the block wrapper around the text.
    pub overhead: usize,
}

/// Condense `text` (whose first line is absolute line `line_start`).
///
/// `anchor` is `(offset, len)` relative to `line_start`; `focus` holds
/// absolute line numbers. Returns `None` when not even the anchor plus one
/// focus line fits.
pub(crate) fn condense(
    text: &str,
    line_start: u32,
    anchor: (u32, u32),
    focus: &[u32],
    context: u32,
    fit: &Fit<'_>,
) -> Option<Condensed> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let n = lines.len();
    if n == 0 {
        return None;
    }
    let fits = |s: &str| fit.counter.count(s) + fit.overhead <= fit.max_tokens;
    let rel_focus: Vec<usize> = focus
        .iter()
        .filter(|&&f| f >= line_start && ((f - line_start) as usize) < n)
        .map(|&f| (f - line_start) as usize)
        .collect();
    let mut base = vec![false; n];
    base[0] = true;
    let a0 = anchor.0 as usize;
    for flag in base.iter_mut().skip(a0).take(anchor.1 as usize) {
        *flag = true;
    }

    if rel_focus.is_empty() {
        // Nothing matched inside: keep a prefix.
        let mut keep = base.clone();
        let mut best = None;
        for i in 0..n {
            keep[i] = true;
            let out = render(&lines, &keep, line_start);
            if !fits(&out) {
                break;
            }
            best = Some(out);
        }
        return best.map(|text| Condensed { text });
    }

    let mut tried = Vec::new();
    for ctx in [context, context / 2, 1, 0] {
        if tried.contains(&ctx) {
            continue;
        }
        tried.push(ctx);
        let mut keep = base.clone();
        for &f in &rel_focus {
            let lo = f.saturating_sub(ctx as usize);
            let hi = (f + ctx as usize).min(n - 1);
            for flag in &mut keep[lo..=hi] {
                *flag = true;
            }
        }
        let out = render(&lines, &keep, line_start);
        if fits(&out) {
            return Some(Condensed { text: out });
        }
    }

    // Even bare focus lines are too many: keep as many as fit, in order.
    let mut keep = base;
    let mut best = None;
    for &f in &rel_focus {
        keep[f] = true;
        let out = render(&lines, &keep, line_start);
        if !fits(&out) {
            break;
        }
        best = Some(out);
    }
    best.map(|text| Condensed { text })
}

/// Kept lines verbatim; each gap as one marker line. A gap of a single
/// blank line is kept as-is instead (a marker would cost more).
fn render(lines: &[&str], keep: &[bool], line_start: u32) -> String {
    let n = lines.len();
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if keep[i] {
            push_line(&mut out, lines[i]);
            i += 1;
            continue;
        }
        let j = (i..n).find(|&k| keep[k]).unwrap_or(n);
        if j == i + 1 && lines[i].trim().is_empty() {
            push_line(&mut out, lines[i]);
        } else {
            let (a, b) = (line_start + i as u32, line_start + j as u32 - 1);
            if a == b {
                out.push_str(&format!("[... line {a} omitted ...]\n"));
            } else {
                out.push_str(&format!("[... lines {a}-{b} omitted ...]\n"));
            }
        }
        i = j;
    }
    out
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::FnCounter;

    fn bytes() -> FnCounter<impl Fn(&str) -> usize + Send + Sync> {
        FnCounter::new("bytes", |s: &str| s.len())
    }

    fn fit(counter: &dyn TokenCounter, max_tokens: usize) -> Fit<'_> {
        Fit {
            counter,
            max_tokens,
            overhead: 0,
        }
    }

    fn numbered(n: u32) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn keeps_anchor_first_line_and_focus_windows() {
        let counter = bytes();
        let text = numbered(30);
        let out = condense(&text, 101, (2, 1), &[115], 1, &fit(&counter, 10_000)).unwrap();
        let t = out.text;
        assert!(t.starts_with("line 1\n"));
        assert!(t.contains("line 3\n"), "anchor kept");
        assert!(t.contains("line 14\nline 15\nline 16\n"), "focus window");
        assert!(
            t.contains("[... lines 102-102 omitted ...]")
                || t.contains("[... line 102 omitted ...]")
        );
        assert!(t.contains("[... lines 117-130 omitted ...]"), "{t}");
        assert!(!t.contains("line 20\n"));
    }

    #[test]
    fn shrinks_context_until_it_fits() {
        let counter = bytes();
        let text = numbered(40);
        let wide = condense(&text, 1, (0, 1), &[20], 5, &fit(&counter, 10_000))
            .unwrap()
            .text;
        let tight = condense(&text, 1, (0, 1), &[20], 5, &fit(&counter, 80))
            .unwrap()
            .text;
        assert!(tight.len() < wide.len());
        assert!(tight.contains("line 20\n"));
        assert!(tight.len() <= 80);
    }

    #[test]
    fn prefix_mode_without_focus_and_none_when_hopeless() {
        let counter = bytes();
        let text = numbered(10);
        let out = condense(&text, 1, (0, 0), &[], 2, &fit(&counter, 60))
            .unwrap()
            .text;
        assert!(out.starts_with("line 1\nline 2\n"));
        assert!(out.len() <= 60);
        assert!(condense(&text, 1, (0, 1), &[5], 2, &fit(&counter, 3)).is_none());
    }

    #[test]
    fn focus_outside_the_span_falls_back_to_a_prefix() {
        let counter = bytes();
        let text = numbered(5);
        let out = condense(&text, 10, (0, 1), &[3, 99], 0, &fit(&counter, 10_000))
            .unwrap()
            .text;
        assert_eq!(out, text, "everything fits, nothing omitted");
    }
}
