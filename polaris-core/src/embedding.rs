#![allow(dead_code)]
use std::sync::{Arc, Mutex};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::error::{PolarisError, Result};

struct ModelInfo {
    fastembed_model: EmbeddingModel,
    pub native_dim: usize,
    /// Dimension used when neither the config nor an existing index sets one.
    default_dim: usize,
    document_prefix: &'static str,
    query_prefix: &'static str,
    /// Built-in `search_min_similarity`; `None` when never calibrated, in which
    /// case search is not gated and the auto-search hook stays silent.
    default_threshold: Option<f32>,
}

/// The model a config that sets none, over no existing index, resolves to.
pub const DEFAULT_MODEL_ID: &str = "nomic-embed-text-v1.5";

/// Every `model_id` [`resolve_model`] accepts, in documentation order.
pub const SUPPORTED_MODELS: &[&str] = &[
    "nomic-embed-text-v1.5",
    "nomic-embed-text-v1.5-quantized",
    "embeddinggemma-300m",
    "mxbai-embed-large-v1",
    "all-minilm-l6-v2",
];

/// The single table of model-dependent facts.
fn resolve_model(model_id: &str) -> Result<ModelInfo> {
    match model_id {
        // 0.63: `polaris eval` over this repo's docs puts on-topic p10 at 0.632
        // and off-topic p95 at 0.628 and recommends their midpoint; it is also
        // the best threshold on the real-query gold set (`eval/run.py`). The
        // classes overlap, so it trades false negatives against false positives.
        "nomic-embed-text-v1.5" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::NomicEmbedTextV15,
            native_dim: 768,
            default_dim: 512,
            document_prefix: "search_document: ",
            query_prefix: "search_query: ",
            default_threshold: Some(0.63),
        }),
        // Same weights, int8-quantized: ~2× faster on CPU, a quarter of the
        // download. Its vectors do not mix with fp32 ones, hence a distinct id.
        // `polaris eval` suggested 0.63–0.64.
        "nomic-embed-text-v1.5-quantized" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::NomicEmbedTextV15Q,
            native_dim: 768,
            default_dim: 512,
            document_prefix: "search_document: ",
            query_prefix: "search_query: ",
            default_threshold: Some(0.63),
        }),
        // Multilingual and Matryoshka-trained; 768 is the measured configuration.
        // 0.42 is the midpoint of the best in-sample thresholds on real queries:
        // 0.40 (this repo, English) and 0.44 (a French/English corpus). Both are
        // in-sample — the weakest number in the table.
        "embeddinggemma-300m" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::EmbeddingGemma300M,
            native_dim: 768,
            default_dim: 768,
            document_prefix: "title: none | text: ",
            query_prefix: "task: search result | query: ",
            default_threshold: Some(0.42),
        }),
        "mxbai-embed-large-v1" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::MxbaiEmbedLargeV1,
            native_dim: 1024,
            default_dim: 1024,
            document_prefix: "",
            query_prefix: "Represent this sentence for searching relevant passages: ",
            default_threshold: None,
        }),
        "all-minilm-l6-v2" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::AllMiniLML6V2,
            native_dim: 384,
            default_dim: 384,
            document_prefix: "",
            query_prefix: "",
            default_threshold: None,
        }),
        _ => Err(PolarisError::Config(format!(
            "Unknown model '{model_id}'. Supported: {}",
            SUPPORTED_MODELS.join(", ")
        ))),
    }
}

/// Returns the native embedding dimension for a model ID, without loading the model.
/// Also validates that the model_id is known.
pub fn native_dim_for(model_id: &str) -> Result<usize> {
    resolve_model(model_id).map(|m| m.native_dim)
}

/// The dimension a model gets when neither the config nor an index sets one.
pub fn default_dim_for(model_id: &str) -> Result<usize> {
    resolve_model(model_id).map(|m| m.default_dim)
}

/// The built-in `search_min_similarity` for a model; `Ok(None)` when uncalibrated.
pub fn default_threshold_for(model_id: &str) -> Result<Option<f32>> {
    resolve_model(model_id).map(|m| m.default_threshold)
}

pub struct EmbeddingEngine {
    model: Mutex<TextEmbedding>,
    target_dim: usize,
    doc_prefix: String,
    query_prefix: String,
}

impl EmbeddingEngine {
    pub fn new(target_dim: usize, model_id: &str) -> Result<Self> {
        let info = resolve_model(model_id)?;
        let cache_dir = crate::paths::polaris_cache_dir()?;
        let model = TextEmbedding::try_new(
            InitOptions::new(info.fastembed_model)
                .with_show_download_progress(true)
                .with_cache_dir(cache_dir),
        )
        .map_err(|e| PolarisError::Embedding(anyhow::anyhow!("Failed to load model: {e}")))?;

        Ok(Self {
            model: Mutex::new(model),
            target_dim,
            doc_prefix: info.document_prefix.to_string(),
            query_prefix: info.query_prefix.to_string(),
        })
    }

    /// Embed a batch of document texts (adds task prefix, truncates + L2-normalizes).
    pub fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("{}{t}", self.doc_prefix))
            .collect();
        self.embed_batch(&prefixed)
    }

    /// Embed a single query string (adds task prefix, truncates + L2-normalizes).
    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let prefixed = format!("{}{query}", self.query_prefix);
        let mut results = self.embed_batch(&[prefixed])?;
        results
            .pop()
            .ok_or_else(|| PolarisError::Embedding(anyhow::anyhow!("Empty embedding result")))
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut model = self
            .model
            .lock()
            .map_err(|e| PolarisError::Embedding(anyhow::anyhow!("Mutex poisoned: {e}")))?;

        let embeddings = model
            .embed(texts.to_vec(), None)
            .map_err(|e| PolarisError::Embedding(anyhow::anyhow!("Embed failed: {e}")))?;

        Ok(embeddings
            .into_iter()
            .map(|emb| truncate_and_normalize(emb, self.target_dim))
            .collect())
    }

    pub fn dim(&self) -> usize {
        self.target_dim
    }
}

/// Slice embedding to `dim` dimensions and L2-normalize.
fn truncate_and_normalize(mut embedding: Vec<f32>, dim: usize) -> Vec<f32> {
    embedding.truncate(dim);

    let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-10 {
        for x in &mut embedding {
            *x /= norm;
        }
    }

    embedding
}

/// Shared, cheap-to-clone handle to an `EmbeddingEngine`.
///
/// Loading the underlying ONNX model is expensive (~140 MB resident).
/// `SharedEmbedding` ensures the model is loaded once and shared across
/// all consumers (typically multiple `Bank` instances).
#[derive(Clone)]
pub struct SharedEmbedding(pub(crate) Arc<EmbeddingEngine>);

impl SharedEmbedding {
    /// Load an embedding model. The first call for a given (model, dim) pair
    /// downloads and caches the model; subsequent calls reuse the cache.
    pub fn load(model_id: &str, dim: usize) -> crate::Result<Self> {
        let engine = EmbeddingEngine::new(dim, model_id)?;
        Ok(Self(Arc::new(engine)))
    }

    /// Borrow the inner engine.
    pub fn engine(&self) -> &EmbeddingEngine {
        &self.0
    }
}

#[cfg(test)]
mod shared_embedding_tests {
    use super::*;

    #[test]
    #[ignore = "downloads ~137 MB ONNX model; run with `cargo test -- --include-ignored`"]
    fn shared_embedding_clone_does_not_reload() {
        // Loading the model is expensive; cloning a SharedEmbedding must be cheap.
        let a = SharedEmbedding::load("nomic-embed-text-v1.5", 64).expect("load");
        let b = a.clone();
        // Both handles should refer to the same Arc<EmbeddingEngine>.
        assert!(Arc::ptr_eq(&a.0, &b.0));
    }

    #[test]
    #[ignore = "downloads ~1.2 GB EmbeddingGemma ONNX model; run with `cargo test -- --include-ignored`"]
    fn embeddinggemma_embeds_queries_and_documents_at_768() {
        let e = SharedEmbedding::load("embeddinggemma-300m", 768).expect("load");
        assert_eq!(e.engine().embed_query("how do I install it").unwrap().len(), 768);
        let docs = e.engine().embed_documents(&["Install with cargo.".to_string()]).unwrap();
        assert_eq!(docs[0].len(), 768);
    }
}

#[cfg(test)]
mod model_resolution_tests {
    use super::*;

    #[test]
    fn quantized_nomic_resolves_like_fp32_nomic() {
        let fp32 = resolve_model("nomic-embed-text-v1.5").unwrap();
        let q = resolve_model("nomic-embed-text-v1.5-quantized").unwrap();
        assert!(matches!(q.fastembed_model, EmbeddingModel::NomicEmbedTextV15Q));
        assert_eq!(q.native_dim, fp32.native_dim);
        assert_eq!(q.default_dim, fp32.default_dim);
        assert_eq!(q.document_prefix, fp32.document_prefix);
        assert_eq!(q.query_prefix, fp32.query_prefix);
        assert_eq!(q.default_threshold, fp32.default_threshold);
    }

    #[test]
    fn unknown_model_error_lists_every_supported_model() {
        let err = native_dim_for("nope").unwrap_err().to_string();
        for id in SUPPORTED_MODELS {
            assert!(err.contains(id), "{id} missing from: {err}");
        }
    }

    #[test]
    fn every_supported_model_resolves_with_a_default_dim_within_native() {
        for id in SUPPORTED_MODELS {
            let info = resolve_model(id).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(
                info.default_dim >= 64 && info.default_dim <= info.native_dim,
                "{id}: default {} outside [64, {}]",
                info.default_dim,
                info.native_dim
            );
        }
    }

    #[test]
    fn default_model_is_supported() {
        assert!(SUPPORTED_MODELS.contains(&DEFAULT_MODEL_ID));
    }

    #[test]
    fn embeddinggemma_uses_its_retrieval_prompts_at_full_dimension() {
        let g = resolve_model("embeddinggemma-300m").unwrap();
        assert!(matches!(g.fastembed_model, EmbeddingModel::EmbeddingGemma300M));
        assert_eq!((g.native_dim, g.default_dim), (768, 768));
        assert_eq!(g.document_prefix, "title: none | text: ");
        assert_eq!(g.query_prefix, "task: search result | query: ");
    }

    #[test]
    fn defaults_match_the_measured_values() {
        assert_eq!(default_dim_for("nomic-embed-text-v1.5").unwrap(), 512);
        assert_eq!(default_dim_for("nomic-embed-text-v1.5-quantized").unwrap(), 512);
        assert_eq!(default_dim_for("embeddinggemma-300m").unwrap(), 768);
        assert_eq!(default_dim_for("mxbai-embed-large-v1").unwrap(), 1024);
        assert_eq!(default_dim_for("all-minilm-l6-v2").unwrap(), 384);

        assert_eq!(default_threshold_for("nomic-embed-text-v1.5").unwrap(), Some(0.63));
        assert_eq!(default_threshold_for("nomic-embed-text-v1.5-quantized").unwrap(), Some(0.63));
        assert_eq!(default_threshold_for("embeddinggemma-300m").unwrap(), Some(0.42));
        assert_eq!(default_threshold_for("mxbai-embed-large-v1").unwrap(), None);
        assert_eq!(default_threshold_for("all-minilm-l6-v2").unwrap(), None);

        assert!(default_dim_for("nope").is_err());
        assert!(default_threshold_for("nope").is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_and_normalize;

    #[test]
    fn truncate_longer_vector() {
        let v = vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
        let result = truncate_and_normalize(v, 3);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn truncate_shorter_vector_noop() {
        // Vec::truncate is a no-op when dim >= len
        let v = vec![1.0f32, 0.0, 0.0];
        let result = truncate_and_normalize(v, 10);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn zero_vector_no_nan() {
        let v = vec![0.0f32, 0.0, 0.0, 0.0];
        let result = truncate_and_normalize(v, 4);
        for x in &result {
            assert!(!x.is_nan(), "zero-vector normalization produced NaN");
        }
    }

    #[test]
    fn unit_vector_stays_unit_length() {
        let v = vec![1.0f32, 0.0, 0.0, 0.0];
        let result = truncate_and_normalize(v, 4);
        let norm: f32 = result.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-6,
            "unit vector norm should be ~1.0, got {norm}"
        );
    }

    #[test]
    fn arbitrary_vector_normalized_correctly() {
        // [3, 4] has norm 5 → normalized [0.6, 0.8]
        let v = vec![3.0f32, 4.0];
        let result = truncate_and_normalize(v, 2);
        assert!((result[0] - 0.6).abs() < 1e-6, "expected 0.6, got {}", result[0]);
        assert!((result[1] - 0.8).abs() < 1e-6, "expected 0.8, got {}", result[1]);
        let norm: f32 = result.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "result should be unit length");
    }
}
