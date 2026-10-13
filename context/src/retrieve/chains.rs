//! Evidence chains: carry relevance along entity-graph paths.
//!
//! A multi-hop question is answered by a path through the graph. "Who
//! founded the company acquired by Alice's employer?" needs the passage
//! linking Alice to her employer, the one recording the acquisition, and
//! the one naming the founder. Only the first shares much vocabulary with
//! the question, so scored on their own the later hops fall below the
//! relevance floor and the model receives half a chain. Here a passage
//! further along a path inherits the relevance of the best passage before
//! it on the same path, decayed per hop.
//!
//! What counts as the next link depends on how the passage was reached. On
//! a path through a relation the question asks about (directly or through
//! its relation class), a passage that *states* the next relation, or
//! *defines* the entity it reaches, is that hop's evidence whatever words
//! it uses ("Juniper bought Copperleaf" answers "acquired"). Any other
//! passage must also cover a question term the earlier passage does not,
//! and never through a bare co-mention: otherwise it is noise that happens
//! to share a name. Lifts whose last relation the question asks about go
//! first, and among those, passages stating the relation beat passages
//! that merely mention an entity on the path.

use std::collections::HashSet;

use super::graph_walk::ChainStep;
use super::query::QuestionRelations;
use super::RankedPassage;
use crate::graph::EdgeKind;

/// Share of relevance a passage inherits per hop along a path.
const CARRY_PER_HOP: f32 = 0.85;
/// Further discount for a hop whose relation the question does not name.
const UNASKED: f32 = 0.9;
/// Most passages lifted for one question, so a hub entity cannot flood
/// the packet.
const MAX_LIFTS: usize = 6;

struct Lift {
    index: usize,
    score: f32,
    /// Answers a part of the question earlier hops did not.
    fresh: bool,
    /// Its relation is one the question asks about.
    asked: bool,
    /// It states the relation or defines the entity reached, rather than
    /// mentioning an entity on the path.
    evidence: bool,
}

/// Raise the scores of passages that continue a relevant graph path.
pub(crate) fn lift_chains(passages: &mut [RankedPassage], asks: &QuestionRelations) {
    let mut lifts: Vec<Lift> = Vec::new();
    for (qi, q) in passages.iter().enumerate() {
        let (Some(q_steps), Some(chain)) = (steps(q), q.chain.as_ref()) else {
            continue;
        };
        let last = &q_steps[q_steps.len() - 1];
        // Evidence for a hop needs no words of its own only on a path the
        // question asks about; otherwise a common name (`len`, `is_empty`)
        // would carry every homonym along.
        let states = matches!(chain.link.as_str(), "states" | "defines");
        let evidence = states && q_steps.iter().any(|s| asks.matches(&s.relation));
        let mut best = 0.0f32;
        for (ai, a) in passages.iter().enumerate() {
            let Some(hops) = hops_from(a, q_steps).filter(|_| ai != qi) else {
                continue;
            };
            if !evidence && (last.kind == Some(EdgeKind::CoMention) || !adds_terms(q, a)) {
                continue;
            }
            best = best.max(a.score * CARRY_PER_HOP.powi(hops as i32));
        }
        let answers = asks.matched(&last.relation);
        let asked = !answers.is_empty();
        if !asked {
            best *= UNASKED;
        }
        // A hop that answers a part of the question its path has not yet
        // covered ("headquartered" after "made" and "acquired") beats one
        // repeating a covered relation (another "acquired" of a hub).
        let covered: HashSet<String> = q_steps[..q_steps.len() - 1]
            .iter()
            .flat_map(|s| asks.matched(&s.relation))
            .collect();
        let fresh = answers.iter().any(|a| !covered.contains(a));
        if best > q.score {
            lifts.push(Lift {
                index: qi,
                score: best,
                fresh,
                asked,
                evidence: states,
            });
        }
    }
    lifts.sort_by(|a, b| {
        (b.fresh, b.asked, b.evidence)
            .cmp(&(a.fresh, a.asked, a.evidence))
            .then(b.score.total_cmp(&a.score))
            .then(a.index.cmp(&b.index))
    });
    for lift in lifts.into_iter().take(MAX_LIFTS) {
        passages[lift.index].score = lift.score;
        passages[lift.index].floor_exempt = true;
    }
}

/// The relation steps that reached `p`, when there are any.
fn steps(p: &RankedPassage) -> Option<&[ChainStep]> {
    p.chain
        .as_ref()
        .map(|c| c.steps.as_slice())
        .filter(|s| !s.is_empty())
}

/// How many hops lead from `a` into a path of `q_steps`, or `None` when
/// `a` does not lead into it. Either `a`'s steps are a prefix of the path
/// (an equally long `a` must state the last step itself), or `a` has no
/// steps and is about the question entity the path starts from: the
/// passage that defines or names the seed is the root of every path out of
/// it.
fn hops_from(a: &RankedPassage, q_steps: &[ChainStep]) -> Option<usize> {
    let chain = a.chain.as_ref()?;
    let a_steps = chain.steps.as_slice();
    if a_steps.is_empty() {
        let first = q_steps.first()?;
        let root = chain.entity == first.from || chain.entity == first.to;
        return root.then_some(q_steps.len());
    }
    if a_steps.len() > q_steps.len() {
        return None;
    }
    let same = a_steps
        .iter()
        .zip(q_steps)
        .all(|(x, y)| x.from == y.from && x.relation == y.relation && x.to == y.to);
    let states = chain.link == "states";
    (same && (a_steps.len() < q_steps.len() || states))
        .then(|| (q_steps.len() - a_steps.len()).max(1))
}

/// True when `q` matches a question term that `a` does not.
fn adds_terms(q: &RankedPassage, a: &RankedPassage) -> bool {
    q.matched_terms.iter().any(|t| !a.matched_terms.contains(t))
}
