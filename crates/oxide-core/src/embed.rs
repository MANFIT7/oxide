//! Optional local semantic embeddings (model2vec / "potion" static vectors).
//!
//! Pure-Rust static embeddings — a token→vector lookup + mean pooling, no ONNX.
//! The model (~30 MB) is downloaded once on first use and cached by the HF hub
//! layer; if it can't be fetched (offline / no model) we simply skip reranking
//! and the TF-IDF + symbol index stands on its own. So this only ever *adds*
//! quality — it never blocks search.

use model2vec_rs::model::StaticModel;
use std::sync::OnceLock;

const REPO: &str = "minishlab/potion-base-8M";

static MODEL: OnceLock<Option<StaticModel>> = OnceLock::new();

fn model() -> Option<&'static StaticModel> {
    MODEL
        .get_or_init(|| StaticModel::from_pretrained(REPO, None, None, None).ok())
        .as_ref()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Semantic-rerank `texts` against `query`. Returns candidate indices ordered
/// best-first, or `None` if the model is unavailable (then keep the prior order).
/// Vectors are L2-normalised by the model, so dot product == cosine similarity.
pub fn rerank(query: &str, texts: &[String]) -> Option<Vec<usize>> {
    if texts.is_empty() {
        return None;
    }
    let m = model()?;
    let q = m.encode_single(query);
    let embs = m.encode(texts);
    let mut scored: Vec<(usize, f32)> = embs
        .iter()
        .enumerate()
        .map(|(i, e)| (i, dot(&q, e)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Some(scored.into_iter().map(|(i, _)| i).collect())
}
