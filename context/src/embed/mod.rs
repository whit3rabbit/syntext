//! Semantic retrieval: host-supplied embeddings as one more ranking signal.
//!
//! The engine never runs a model itself. The host implements [`Embedder`]
//! with whatever embedding model it already has (a small sentence encoder
//! on CPU, an in-process runtime, an embeddings endpoint). The store keeps
//! compact 8-bit vectors in a sidecar file beside each segment, keyed by a
//! hash of the exact text embedded, so a passage is embedded once per model
//! however often its document is re-synced or compacted. Retrieval adds the
//! passages nearest the question as candidates and scores every
//! candidate's similarity as a reranker signal next to the lexical, graph,
//! and exact ones.
//!
//! Embedding is a separate, resumable job ([`ContextStore::embed`]), never
//! part of a sync: a CPU encoder manages tens of passages a second, so a
//! large tree takes minutes. Queries use whatever has been embedded so far.
//!
//! [`ContextStore::embed`]: crate::store::ContextStore::embed

pub(crate) mod job;
pub(crate) mod registry;
pub(crate) mod table;

use xxhash_rust::xxh64::xxh64;

pub use job::{EmbedOptions, EmbedProgress, EmbedReport};

/// Turns text into vectors for semantic retrieval, with the host's model.
///
/// Implementations must be deterministic for a given [`id`](Self::id):
/// vectors stored by one run are compared with questions embedded by
/// another.
pub trait Embedder: Send + Sync {
    /// The model and every setting that changes its vectors, e.g.
    /// `"bge-small-en-v1.5"`. Vectors are only compared with vectors of the
    /// same id; a new id means re-embedding.
    fn id(&self) -> &str;

    /// Length of every vector.
    fn dims(&self) -> usize;

    /// One vector per passage text, in order.
    fn embed_documents(&self, texts: &[&str]) -> std::result::Result<Vec<Vec<f32>>, String>;

    /// The vector of a question. Defaults to [`embed_documents`]; models
    /// that prefix queries (`"query: "`, an instruction) override it.
    ///
    /// [`embed_documents`]: Self::embed_documents
    fn embed_query(&self, text: &str) -> std::result::Result<Vec<f32>, String> {
        let mut v = self.embed_documents(&[text])?;
        v.pop()
            .ok_or_else(|| "embedder returned no vector".to_string())
    }
}

/// Adapts a closure into an [`Embedder`]: `f(texts, is_query)` returns one
/// vector per text.
pub struct FnEmbedder<F> {
    id: String,
    dims: usize,
    f: F,
}

impl<F> FnEmbedder<F>
where
    F: Fn(&[&str], bool) -> std::result::Result<Vec<Vec<f32>>, String> + Send + Sync,
{
    /// Wrap `f`, a model producing `dims`-long vectors, identified by `id`.
    pub fn new(id: impl Into<String>, dims: usize, f: F) -> Self {
        FnEmbedder {
            id: id.into(),
            dims,
            f,
        }
    }
}

impl<F> Embedder for FnEmbedder<F>
where
    F: Fn(&[&str], bool) -> std::result::Result<Vec<Vec<f32>>, String> + Send + Sync,
{
    fn id(&self) -> &str {
        &self.id
    }

    fn dims(&self) -> usize {
        self.dims
    }

    fn embed_documents(&self, texts: &[&str]) -> std::result::Result<Vec<Vec<f32>>, String> {
        (self.f)(texts, false)
    }

    fn embed_query(&self, text: &str) -> std::result::Result<Vec<f32>, String> {
        let mut v = (self.f)(&[text], true)?;
        v.pop()
            .ok_or_else(|| "embedder returned no vector".to_string())
    }
}

/// Most characters of a passage that are embedded (about 512 tokens).
const MAX_EMBED_CHARS: usize = 2000;
/// Longest accepted embedder id, in bytes.
pub(crate) const MAX_ID_BYTES: usize = 256;
/// Largest accepted vector length.
pub(crate) const MAX_DIMS: usize = 4096;

/// The text embedded for a passage: where it is (file, section, symbol)
/// and what it says, so the same wording in an unrelated place does not
/// look alike.
pub(crate) fn passage_text(breadcrumb: &str, symbol: Option<&str>, body: &str) -> String {
    let mut out = String::with_capacity(breadcrumb.len() + body.len().min(MAX_EMBED_CHARS) + 8);
    out.push_str(breadcrumb);
    if let Some(s) = symbol.filter(|s| !breadcrumb.ends_with(s)) {
        out.push_str(" > ");
        out.push_str(s);
    }
    out.push('\n');
    out.extend(body.trim().chars().take(MAX_EMBED_CHARS));
    out
}

/// Hash identifying embedded text; never 0, which marks an absent row.
pub(crate) fn text_hash(text: &str) -> u64 {
    xxh64(text.as_bytes(), 0x5e3a).max(1)
}

/// File-name key of an embedder id.
pub(crate) fn embedder_key(id: &str) -> String {
    format!("{:016x}", xxh64(id.as_bytes(), 0))
}

/// Validate what an embedder returned for `n` texts.
pub(crate) fn check_vectors(
    vectors: &[Vec<f32>],
    n: usize,
    dims: usize,
) -> std::result::Result<(), String> {
    if vectors.len() != n {
        return Err(format!(
            "embedder returned {} vectors for {n} texts",
            vectors.len()
        ));
    }
    match vectors.iter().find(|v| v.len() != dims) {
        Some(v) => Err(format!(
            "embedder returned a {}-long vector, expected {dims}",
            v.len()
        )),
        None => Ok(()),
    }
}

/// Unit-normalize `v` and quantize it to `i8` with one scale: `v[i] ≈
/// scale * q[i]`. `None` for a zero or non-finite vector.
pub(crate) fn quantize(v: &[f32]) -> Option<(f32, Vec<i8>)> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return None;
    }
    let max = v.iter().map(|x| (x / norm).abs()).fold(0.0f32, f32::max);
    if max == 0.0 || !max.is_finite() {
        return None;
    }
    let scale = max / 127.0;
    let q = v
        .iter()
        .map(|x| ((x / norm) / scale).round().clamp(-127.0, 127.0) as i8)
        .collect();
    Some((scale, q))
}

/// Integer dot product of two quantized vectors.
pub(crate) fn dot(a: &[i8], b: &[i8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| i32::from(x) * i32::from(y))
        .sum()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
