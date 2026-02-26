#![allow(dead_code)]
use std::sync::Mutex;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use crate::error::{PolarisError, Result};

const DOCUMENT_PREFIX: &str = "search_document: ";
const QUERY_PREFIX: &str = "search_query: ";

pub struct EmbeddingEngine {
    model: Mutex<TextEmbedding>,
    target_dim: usize,
}

impl EmbeddingEngine {
    pub fn new(target_dim: usize) -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::NomicEmbedTextV15)
                .with_show_download_progress(true),
        )
        .map_err(|e| PolarisError::Embedding(anyhow::anyhow!("Failed to load model: {e}")))?;

        Ok(Self { model: Mutex::new(model), target_dim })
    }

    /// Embed a batch of document texts (adds task prefix, truncates + L2-normalizes).
    pub fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("{DOCUMENT_PREFIX}{t}"))
            .collect();
        self.embed_batch(&prefixed)
    }

    /// Embed a single query string (adds task prefix, truncates + L2-normalizes).
    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let prefixed = format!("{QUERY_PREFIX}{query}");
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
