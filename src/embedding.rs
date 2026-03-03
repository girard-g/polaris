#![allow(dead_code)]
use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::error::{PolarisError, Result};

struct ModelInfo {
    fastembed_model: EmbeddingModel,
    pub native_dim: usize,
    document_prefix: &'static str,
    query_prefix: &'static str,
}

fn resolve_model(model_id: &str) -> Result<ModelInfo> {
    match model_id {
        "nomic-embed-text-v1.5" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::NomicEmbedTextV15,
            native_dim: 768,
            document_prefix: "search_document: ",
            query_prefix: "search_query: ",
        }),
        "mxbai-embed-large-v1" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::MxbaiEmbedLargeV1,
            native_dim: 1024,
            document_prefix: "",
            query_prefix: "Represent this sentence for searching relevant passages: ",
        }),
        "all-minilm-l6-v2" => Ok(ModelInfo {
            fastembed_model: EmbeddingModel::AllMiniLML6V2,
            native_dim: 384,
            document_prefix: "",
            query_prefix: "",
        }),
        _ => Err(PolarisError::Config(format!(
            "Unknown model '{}'. Supported: nomic-embed-text-v1.5, mxbai-embed-large-v1, all-minilm-l6-v2",
            model_id
        ))),
    }
}

/// Returns the native embedding dimension for a model ID, without loading the model.
/// Also validates that the model_id is known.
pub fn native_dim_for(model_id: &str) -> Result<usize> {
    resolve_model(model_id).map(|m| m.native_dim)
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
        let model = TextEmbedding::try_new(
            InitOptions::new(info.fastembed_model)
                .with_show_download_progress(true),
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
