//! Local embedding provider backed by [`fastembed`].
//!
//! This module is only compiled when the `local-embeddings` feature is enabled.
//! It uses the fastembed-rs crate to run embedding models entirely on-device,
//! requiring no external API calls.

#![cfg(feature = "local-embeddings")]

use super::{EmbeddingError, EmbeddingProvider};

/// A local (on-device) embedding provider using fastembed-rs.
pub struct LocalEmbeddingProvider {
    model: fastembed::TextEmbedding,
    dimensions: usize,
}

impl LocalEmbeddingProvider {
    /// Create a new local embedding provider.
    ///
    /// # Arguments
    /// * `model_name` - The name of the embedding model (e.g. "all-MiniLM-L6-v2").
    /// * `dimensions` - The expected embedding dimensionality (384 for all-MiniLM-L6-v2).
    pub fn new(model_name: &str, dimensions: usize) -> Result<Self, EmbeddingError> {
        let embedding_model = resolve_model(model_name)?;

        let options = fastembed::InitOptions::new(embedding_model)
            .with_show_download_progress(true);

        let model = fastembed::TextEmbedding::try_new(options)
            .map_err(|e| EmbeddingError::Model(e.to_string()))?;

        Ok(Self { model, dimensions })
    }

    /// Validate that every embedding in the batch has the expected number of
    /// dimensions. Returns the batch unchanged on success.
    fn validate_dimensions(
        &self,
        embeddings: Vec<Vec<f32>>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        for embedding in &embeddings {
            if embedding.len() != self.dimensions {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: self.dimensions,
                    actual: embedding.len(),
                });
            }
        }
        Ok(embeddings)
    }
}

impl EmbeddingProvider for LocalEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let texts = vec![text.to_string()];
        let mut results = self
            .model
            .embed(texts, None)
            .map_err(|e| EmbeddingError::Model(e.to_string()))?;

        let results = self.validate_dimensions(std::mem::take(&mut results))?;

        results
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::Model("Model returned no embeddings".to_string()))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
        let results = self
            .model
            .embed(owned, None)
            .map_err(|e| EmbeddingError::Model(e.to_string()))?;

        self.validate_dimensions(results)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a human-readable model name to the fastembed enum variant.
fn resolve_model(name: &str) -> Result<fastembed::EmbeddingModel, EmbeddingError> {
    match name {
        "all-MiniLM-L6-v2" => Ok(fastembed::EmbeddingModel::AllMiniLML6V2),
        "all-MiniLM-L12-v2" => Ok(fastembed::EmbeddingModel::AllMiniLML12V2),
        "BGE-small-en-v1.5" | "bge-small-en-v1.5" => {
            Ok(fastembed::EmbeddingModel::BGESmallENV15)
        }
        "BGE-base-en-v1.5" | "bge-base-en-v1.5" => {
            Ok(fastembed::EmbeddingModel::BGEBaseENV15)
        }
        other => Err(EmbeddingError::Model(format!(
            "Unsupported local embedding model: '{other}'. \
             Supported models: all-MiniLM-L6-v2, all-MiniLM-L12-v2, \
             bge-small-en-v1.5, bge-base-en-v1.5"
        ))),
    }
}
