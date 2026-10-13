//! Token counting for budget enforcement.
//!
//! The engine never needs token ids, only counts, so any tokenizer can be
//! adapted. Final prompt fit must come from the target model's own tokenizer
//! and chat template; [`HeuristicCounter`] exists so the engine works before
//! a tokenizer is wired in, and it errs toward over-counting.

use std::fmt;

/// Counts tokens the way the target model's tokenizer does.
pub trait TokenCounter: Send + Sync {
    /// Number of tokens `text` encodes to (without template overhead).
    fn count(&self, text: &str) -> usize;

    /// Identifier recorded in receipts, e.g. `"qwen3"` or `"heuristic-v1"`.
    fn name(&self) -> &str;
}

/// Adapts a closure into a [`TokenCounter`]:
/// `FnCounter::new("llama3", |s| tokenizer.encode(s).len())`.
pub struct FnCounter<F> {
    name: String,
    f: F,
}

impl<F> FnCounter<F>
where
    F: Fn(&str) -> usize + Send + Sync,
{
    /// Wrap `f`, reporting `name` in receipts.
    pub fn new(name: impl Into<String>, f: F) -> Self {
        FnCounter {
            name: name.into(),
            f,
        }
    }
}

impl<F> TokenCounter for FnCounter<F>
where
    F: Fn(&str) -> usize + Send + Sync,
{
    fn count(&self, text: &str) -> usize {
        (self.f)(text)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl<F> fmt::Debug for FnCounter<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnCounter")
            .field("name", &self.name)
            .finish()
    }
}

/// Tokenizer-free estimate tuned to over-count modern BPE vocabularies
/// (Llama 3, Qwen, GPT-4 class) by roughly 0-20% on English and code.
///
/// Rules: an ASCII letter run costs one token per five letters (rounded up);
/// a digit run one per three digits; a single space is free (BPE merges it
/// into the next token); longer space runs cost one per four; each newline,
/// ASCII symbol, and non-ASCII character costs one; pictographs cost two.
#[derive(Debug, Clone)]
pub struct HeuristicCounter {
    scale: f64,
}

impl HeuristicCounter {
    /// Estimator with no extra safety factor.
    pub fn new() -> Self {
        HeuristicCounter { scale: 1.0 }
    }

    /// Estimator whose counts are multiplied by `scale` (clamped to at least
    /// 1.0), for hosts that want additional headroom.
    pub fn with_scale(scale: f64) -> Self {
        let scale = if scale.is_finite() {
            scale.max(1.0)
        } else {
            1.0
        };
        HeuristicCounter { scale }
    }
}

impl Default for HeuristicCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenCounter for HeuristicCounter {
    fn count(&self, text: &str) -> usize {
        let raw = estimate(text);
        if self.scale == 1.0 {
            raw
        } else {
            (raw as f64 * self.scale).ceil() as usize
        }
    }

    fn name(&self) -> &str {
        "heuristic-v1"
    }
}

fn estimate(text: &str) -> usize {
    let mut tokens = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphabetic() {
            let len = 1 + take_run(&mut chars, |n| n.is_ascii_alphabetic());
            tokens += len.div_ceil(5);
        } else if c.is_ascii_digit() {
            let len = 1 + take_run(&mut chars, |n| n.is_ascii_digit());
            tokens += len.div_ceil(3);
        } else if c == ' ' || c == '\t' {
            let len = 1 + take_run(&mut chars, |n| n == ' ' || n == '\t');
            if len > 1 {
                tokens += len.div_ceil(4);
            }
        } else if c == '\r' {
            // Counted with the newline it precedes.
        } else if (c as u32) >= 0x1F000 {
            tokens += 2;
        } else {
            tokens += 1;
        }
    }
    tokens
}

fn take_run(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    pred: impl Fn(char) -> bool,
) -> usize {
    let mut n = 0;
    while chars.peek().is_some_and(|&c| pred(c)) {
        chars.next();
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_sentence_is_close_to_bpe() {
        let c = HeuristicCounter::new();
        // 10 tokens in cl100k / Llama 3.
        assert_eq!(c.count("The quick brown fox jumps over the lazy dog."), 10);
    }

    #[test]
    fn code_over_counts_rather_than_under() {
        let c = HeuristicCounter::new();
        // About 14-16 tokens in real BPE vocabularies.
        let n = c.count("fn parse_query(args: &str) -> Result<Query> {");
        assert!((15..=22).contains(&n), "{n}");
    }

    #[test]
    fn whitespace_and_symbols() {
        let c = HeuristicCounter::new();
        assert_eq!(c.count(""), 0);
        assert_eq!(c.count("a b"), 2);
        assert_eq!(c.count("\n\n"), 2);
        assert_eq!(c.count("        x"), 3);
        assert_eq!(c.count("検索"), 2);
        assert_eq!(c.count("12345"), 2);
        assert_eq!(c.count("\u{1F600}"), 2);
    }

    #[test]
    fn scale_is_applied_and_clamped() {
        let text = "The quick brown fox jumps over the lazy dog.";
        assert_eq!(HeuristicCounter::with_scale(1.5).count(text), 15);
        assert_eq!(HeuristicCounter::with_scale(0.1).count(text), 10);
        assert_eq!(HeuristicCounter::with_scale(f64::NAN).count(text), 10);
    }

    #[test]
    fn closures_adapt_into_counters() {
        let c = FnCounter::new("words", |s: &str| s.split_whitespace().count());
        assert_eq!(c.count("one two three"), 3);
        assert_eq!(c.name(), "words");
    }
}
