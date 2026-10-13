//! Plain-text and Markdown reports.

use std::fmt::Write as _;

use super::{EvalSet, QuestionResult, Summary};

/// One Markdown table row per set and window.
pub fn table(summaries: &[Summary]) -> String {
    let mut out = String::from(
        "| Set | Window | Questions | All supports in prompt | Support recall | All retrieved | MRR | Evidence tokens | p50 / p95 ms |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    );
    for s in summaries {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} ({:.0}%) | {:.0}% | {} | {:.2} | {:.0} | {:.1} / {:.1} |",
            s.set,
            s.window,
            s.questions,
            s.all_supported,
            100.0 * s.rate(),
            100.0 * s.support_recall,
            s.retrieved_all,
            s.mrr,
            s.mean_evidence_tokens,
            s.p50_ms,
            s.p95_ms,
        );
    }
    out
}

/// Questions that missed a support, with the supports they missed.
pub fn failures(sets: &[EvalSet], results: &[QuestionResult]) -> String {
    let mut out = String::new();
    for r in results.iter().filter(|r| !r.all_supported()) {
        let Some(q) = sets
            .iter()
            .find(|s| s.name == r.set)
            .and_then(|s| s.questions.iter().find(|q| q.id == r.id))
        else {
            continue;
        };
        let _ = writeln!(
            out,
            "{} {} @{}: {}  (first support at rank {}, retrieved all: {})",
            r.set,
            r.id,
            r.window,
            q.question,
            r.first_rank.map_or("-".to_string(), |k| k.to_string()),
            r.retrieved_all,
        );
        if let Some(e) = &r.error {
            let _ = writeln!(out, "    error: {e}");
        }
        for (s, found) in q.supports.iter().zip(&r.supports) {
            if !found {
                let _ = writeln!(out, "    missing {}: {:?}", s.doc, s.contains);
            }
        }
    }
    out
}
