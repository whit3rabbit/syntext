//! Evidence chains: carry relevance along entity-graph paths.
//!
//! A multi-hop question is answered by a path through the graph. "Who
//! founded the company acquired by Alice's employer?" needs the passage
//! linking Alice to her employer, the one recording the acquisition, and
//! the one naming the founder. Only the first shares much vocabulary with
//! the question, so scored on their own the later hops fall below the
//! relevance floor and the model receives half a chain. Here a passage
//! further along a path inherits the relevance of the best passage before
//! it on the same path, decayed per hop, but only when it covers question
//! terms that passage does not: a passage that adds nothing new is a
//! mention, not the next link of the answer.

use super::graph_walk::ChainStep;
use super::RankedPassage;

/// Share of relevance a passage inherits per hop along a path.
const CARRY_PER_HOP: f32 = 0.85;
/// Most passages lifted for one question, so a hub entity cannot flood
/// the packet.
const MAX_LIFTS: usize = 6;

/// Raise the scores of passages that continue a relevant graph path.
pub(crate) fn lift_chains(passages: &mut [RankedPassage]) {
    let mut lifts: Vec<(usize, f32)> = Vec::new();
    for (qi, q) in passages.iter().enumerate() {
        let Some(q_steps) = steps(q) else {
            continue;
        };
        let mut best = 0.0f32;
        for (ai, a) in passages.iter().enumerate() {
            let Some(a_steps) = steps(a) else {
                continue;
            };
            if ai == qi || !continues(a, a_steps, q_steps) || !adds_terms(q, a) {
                continue;
            }
            let hops = (q_steps.len() - a_steps.len()).max(1) as i32;
            best = best.max(a.score * CARRY_PER_HOP.powi(hops));
        }
        if best > q.score {
            lifts.push((qi, best));
        }
    }
    lifts.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (i, score) in lifts.into_iter().take(MAX_LIFTS) {
        passages[i].score = score;
    }
}

/// The relation steps that reached `p`, when there are any.
fn steps(p: &RankedPassage) -> Option<&[ChainStep]> {
    p.chain
        .as_ref()
        .map(|c| c.steps.as_slice())
        .filter(|s| !s.is_empty())
}

/// True when `q`'s path extends `a`'s: `a`'s steps are a prefix of `q`'s,
/// and an equally long `a` is the passage stating the last step itself.
fn continues(a: &RankedPassage, a_steps: &[ChainStep], q_steps: &[ChainStep]) -> bool {
    if a_steps.len() > q_steps.len() {
        return false;
    }
    let same = a_steps
        .iter()
        .zip(q_steps)
        .all(|(x, y)| x.from == y.from && x.relation == y.relation && x.to == y.to);
    let states = a.chain.as_ref().is_some_and(|c| c.link == "states");
    same && (a_steps.len() < q_steps.len() || states)
}

/// True when `q` matches a question term that `a` does not.
fn adds_terms(q: &RankedPassage, a: &RankedPassage) -> bool {
    q.matched_terms.iter().any(|t| !a.matched_terms.contains(t))
}
