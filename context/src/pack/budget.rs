//! Token budget of one model call.

use serde::{Deserialize, Serialize};

use crate::render::ChatMessage;
use crate::tokens::TokenCounter;

/// Tokens a chat template adds around each message (role markers,
/// separators). Conservative for ChatML, Llama 3, Gemma, and Mistral
/// templates.
pub const MESSAGE_OVERHEAD: usize = 8;

/// How a model's context window is spent. Evidence gets what is left after
/// the system prompt, history, question, answer reserve, and margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Model context window.
    pub window: usize,
    /// System prompt (including template overhead).
    pub system: usize,
    /// Conversation history kept in the prompt.
    pub history: usize,
    /// The question.
    pub question: usize,
    /// Reserved for the generated answer.
    pub generation: usize,
    /// Safety margin for template overhead and tokenizer drift.
    pub margin: usize,
}

impl Budget {
    /// Defaults for a window: a quarter of it (256..=2048 tokens, at most
    /// half the window) reserved for the answer, and a 1/32 margin (at
    /// least 32 tokens).
    pub fn for_window(window: usize) -> Self {
        Budget {
            window,
            system: 0,
            history: 0,
            question: 0,
            generation: (window / 4).clamp(256, 2048).min(window / 2),
            margin: (window / 32).max(32),
        }
    }

    /// Set the answer reserve.
    pub fn with_generation(mut self, tokens: usize) -> Self {
        self.generation = tokens;
        self
    }

    /// Set the safety margin.
    pub fn with_margin(mut self, tokens: usize) -> Self {
        self.margin = tokens;
        self
    }

    /// Count the system prompt, history, and question with the model's
    /// tokenizer (plus per-message template overhead).
    pub fn measure(
        mut self,
        counter: &dyn TokenCounter,
        system: Option<&str>,
        history: &[ChatMessage],
        question: &str,
    ) -> Self {
        self.system = system.map_or(0, |s| counter.count(s) + MESSAGE_OVERHEAD);
        self.history = history
            .iter()
            .map(|m| counter.count(&m.content) + MESSAGE_OVERHEAD)
            .sum();
        self.question = counter.count(question) + MESSAGE_OVERHEAD;
        self
    }

    /// Tokens available for evidence.
    pub fn evidence_tokens(&self) -> usize {
        self.window
            .saturating_sub(self.system)
            .saturating_sub(self.history)
            .saturating_sub(self.question)
            .saturating_sub(self.generation)
            .saturating_sub(self.margin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::HeuristicCounter;

    #[test]
    fn defaults_scale_with_the_window() {
        let b = Budget::for_window(8192);
        assert_eq!((b.generation, b.margin), (2048, 256));
        assert_eq!(b.evidence_tokens(), 8192 - 2048 - 256);
        let b = Budget::for_window(4096);
        assert_eq!((b.generation, b.margin), (1024, 128));
        let b = Budget::for_window(512);
        assert_eq!(b.generation, 256);
        assert_eq!(Budget::for_window(100).evidence_tokens(), 100 - 50 - 32);
        assert_eq!(
            Budget::for_window(60).evidence_tokens(),
            0,
            "tiny window saturates"
        );
    }

    #[test]
    fn measure_counts_every_part() {
        let c = HeuristicCounter::new();
        let history = vec![
            ChatMessage::user("hello there"),
            ChatMessage::assistant("hi"),
        ];
        let b = Budget::for_window(4096).measure(&c, Some("Be brief."), &history, "What now?");
        assert_eq!(b.system, c.count("Be brief.") + MESSAGE_OVERHEAD);
        assert_eq!(
            b.history,
            c.count("hello there") + c.count("hi") + 2 * MESSAGE_OVERHEAD
        );
        assert!(b.evidence_tokens() < Budget::for_window(4096).evidence_tokens());
    }
}
