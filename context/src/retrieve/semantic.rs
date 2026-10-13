//! Semantic candidates: the passages nearest the question by vector.
//!
//! An exact scan over every visible passage's 8-bit vector. At 384
//! dimensions that is a few milliseconds per hundred thousand passages, so
//! an approximate index would only pay off near a million.

use std::sync::Arc;

use super::candidates::Visibility;
use crate::embed::registry::Tables;
use crate::embed::table::VectorTable;
use crate::embed::{dot, quantize, Embedder};
use crate::error::{ContextError, Result};
use crate::store::{PassageLoc, StoreSnapshot};

/// The question's vector against one snapshot's sidecars.
pub(crate) struct QuerySims {
    scale: f32,
    q: Vec<i8>,
    /// Table of each snapshot segment, by segment index.
    tables: Vec<Option<Arc<VectorTable>>>,
}

impl QuerySims {
    /// Embed `question`; `None` when no segment has vectors for this
    /// embedder yet (nothing to compare with, so no model call).
    pub(crate) fn new(
        snap: &StoreSnapshot,
        embedder: &dyn Embedder,
        tables: &Tables,
        question: &str,
    ) -> Result<Option<Self>> {
        let by_segment: Vec<Option<Arc<VectorTable>>> = snap
            .segments
            .iter()
            .map(|s| tables.get(&s.name).cloned())
            .collect();
        if by_segment.iter().all(Option::is_none) {
            return Ok(None);
        }
        let v = embedder
            .embed_query(question)
            .map_err(ContextError::Embedder)?;
        if v.len() != embedder.dims() {
            return Err(ContextError::Embedder(format!(
                "query vector has {} dimensions, expected {}",
                v.len(),
                embedder.dims()
            )));
        }
        Ok(quantize(&v).map(|(scale, q)| QuerySims {
            scale,
            q,
            tables: by_segment,
        }))
    }

    /// Cosine similarity of the question and the passage at `loc`.
    pub(crate) fn sim(&self, loc: PassageLoc) -> Option<f32> {
        let t = self.tables.get(loc.seg as usize)?.as_ref()?;
        let (_, scale, v) = t.get(loc.pid as usize)?;
        (v.len() == self.q.len()).then(|| dot(&self.q, v) as f32 * scale * self.scale)
    }

    /// The `k` visible passages most similar to the question, best first.
    pub(crate) fn nearest(
        &self,
        snap: &StoreSnapshot,
        vis: &Visibility,
        k: usize,
    ) -> Vec<(PassageLoc, f32)> {
        let mut all: Vec<(PassageLoc, f32)> = Vec::new();
        for (si, t) in self.tables.iter().enumerate() {
            let Some(t) = t else {
                continue;
            };
            for pid in 0..t.rows() {
                let loc = PassageLoc {
                    seg: si as u32,
                    pid: pid as u32,
                };
                if t.get(pid).is_none() || !vis.passage(snap, loc) {
                    continue;
                }
                if let Some(s) = self.sim(loc) {
                    all.push((loc, s));
                }
            }
        }
        let by_sim = |a: &(PassageLoc, f32), b: &(PassageLoc, f32)| {
            b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0))
        };
        if all.len() > k && k > 0 {
            all.select_nth_unstable_by(k - 1, by_sim);
            all.truncate(k);
        }
        all.sort_by(by_sim);
        all
    }
}

/// Map raw similarities onto 0-1 within this question's neighbourhood: the
/// nearest passage scores 1, the `k`-th nearest 0. Raw cosines of sentence
/// encoders sit in a narrow band (0.6-0.8 is typical), so they would barely
/// separate candidates as they are.
pub(crate) struct SimScale {
    lo: f32,
    hi: f32,
}

impl SimScale {
    pub(crate) fn new(nearest: &[(PassageLoc, f32)]) -> Self {
        let hi = nearest.first().map_or(1.0, |n| n.1);
        let lo = nearest.last().map_or(0.0, |n| n.1);
        SimScale { lo, hi }
    }

    pub(crate) fn normalize(&self, sim: f32) -> f32 {
        if self.hi - self.lo <= f32::EPSILON {
            return if sim >= self.hi { 1.0 } else { 0.0 };
        }
        ((sim - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
    }
}
