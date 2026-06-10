//! Optional local semantic embeddings (model2vec / "potion" static vectors).
//!
//! Pure-Rust static embeddings — a token→vector lookup + mean pooling, no ONNX.
//! The model (~30 MB) is downloaded once on first use and cached by the HF hub
//! layer; if it can't be fetched (offline / no model) we simply skip reranking
//! and the TF-IDF + symbol index stands on its own. So this only ever *adds*
//! quality — it never blocks search.

use model2vec_rs::model::StaticModel;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

const REPO: &str = "minishlab/potion-base-8M";

static MODEL: OnceLock<StaticModel> = OnceLock::new();
// 0 = not started, 1 = loading (background), 2 = ready, 3 = failed.
static STATE: AtomicU8 = AtomicU8::new(0);

/// Non-blocking model access. The ~30MB download (first run) happens on a
/// DETACHED thread: a stalled fetch must never block `codebase_search` — the
/// hf-hub read has no timeout, and a blocked OnceLock init would make every
/// later search burn its full budget and leak a thread. Until the model is
/// ready, callers just skip the semantic rerank (TF-IDF order stands).
fn model() -> Option<&'static StaticModel> {
    match STATE.load(Ordering::Acquire) {
        2 => MODEL.get(),
        3 => None,
        _ => {
            if STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                std::thread::spawn(|| {
                    match StaticModel::from_pretrained(REPO, None, None, None) {
                        Ok(m) => {
                            let _ = MODEL.set(m);
                            STATE.store(2, Ordering::Release);
                        }
                        Err(_) => STATE.store(3, Ordering::Release),
                    }
                });
            }
            None
        }
    }
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
