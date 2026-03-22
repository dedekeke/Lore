//! OpenAI API embedding provider.
//!
//! Calls the OpenAI `/v1/embeddings` endpoint to generate text embeddings.
//! Requires a valid API key.

use serde::{Deserialize, Serialize};

use super::{EmbeddingError, EmbeddingProvider};

/// An embedding provider that delegates to the OpenAI Embeddings API.
pub struct OpenAiEmbeddingProvider {
    api_key: String,
    model: String,
    dimensions: usize,
    client: reqwest::Client,
}

impl OpenAiEmbeddingProvider {
    /// Create a new OpenAI embedding provider.
    ///
    /// # Arguments
    /// * `api_key`    - OpenAI API key.
    /// * `model`      - Model identifier (e.g. "text-embedding-3-small").
    /// * `dimensions` - Expected embedding dimensionality.
    pub fn new(api_key: &str, model: &str, dimensions: usize) -> Self {
        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimensions,
            client: reqwest::Client::new(),
        }
    }

    /// Send an embedding request to the OpenAI API.
    async fn request_embeddings(
        &self,
        input: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let body = EmbeddingRequest {
            model: &self.model,
            input: &input,
            dimensions: Some(self.dimensions),
        };

        let response = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbeddingError::Api(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(EmbeddingError::Api(format!(
                "OpenAI API returned {status}: {text}"
            )));
        }

        let result: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::Api(format!("Failed to parse OpenAI response: {e}")))?;

        // Sort by index to ensure ordering matches the input order.
        let mut data = result.data;
        data.sort_by_key(|d| d.index);

        let embeddings: Vec<Vec<f32>> = data.into_iter().map(|d| d.embedding).collect();

        // Validate dimensions.
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

impl EmbeddingProvider for OpenAiEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let mut results = self.request_embeddings(vec![text.to_string()]).await?;

        results
            .pop()
            .ok_or_else(|| EmbeddingError::Api("OpenAI returned no embeddings".to_string()))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let input = texts.iter().map(|t| t.to_string()).collect();
        self.request_embeddings(input).await
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

// ---------------------------------------------------------------------------
// API request / response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}
