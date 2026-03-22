use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{EmbeddingError, EmbeddingProvider};

pub struct OpenAiEmbeddingProvider {
    api_key: String,
    model: String,
    dimensions: usize,
    client: reqwest::Client,
}

impl OpenAiEmbeddingProvider {
    pub fn new(api_key: &str, model: &str, dimensions: usize) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimensions,
            client,
        }
    }

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
                "OpenAI API returned {status} for model '{}': {text}. \
                 Note: the 'dimensions' parameter is only supported by text-embedding-3-* models.",
                self.model
            )));
        }

        let result: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::Api(format!("Failed to parse OpenAI response: {e}")))?;

        // OpenAI does not guarantee ordering; sort by index.
        let mut data = result.data;
        data.sort_by_key(|d| d.index);

        let embeddings: Vec<Vec<f32>> = data.into_iter().map(|d| d.embedding).collect();

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
        let results = self.request_embeddings(vec![text.to_string()]).await?;

        results
            .into_iter()
            .next()
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
