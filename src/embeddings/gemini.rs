use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{EmbeddingError, EmbeddingProvider};

pub struct GeminiEmbeddingProvider {
    api_key: String,
    model: String,
    dimensions: usize,
    client: reqwest::Client,
}

impl GeminiEmbeddingProvider {
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

    fn embed_url(&self) -> String {
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:embedContent",
            self.model
        )
    }

    fn batch_url(&self) -> String {
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:batchEmbedContents",
            self.model
        )
    }

    async fn request_embedding(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let body = EmbedContentRequest {
            content: Content {
                parts: vec![Part {
                    text: text.to_string(),
                }],
            },
            output_dimensionality: Some(self.dimensions),
        };

        let response = self
            .client
            .post(self.embed_url())
            .header("x-goog-api-key", &self.api_key)
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
                "Gemini API returned {status}: {text}"
            )));
        }

        let result: EmbedContentResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::Api(format!("Failed to parse Gemini response: {e}")))?;

        let embedding = result.embedding.values;
        if embedding.len() != self.dimensions {
            return Err(EmbeddingError::DimensionMismatch {
                expected: self.dimensions,
                actual: embedding.len(),
            });
        }

        Ok(embedding)
    }

    async fn request_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let model_path = format!("models/{}", self.model);
        let requests: Vec<BatchEmbedRequest> = texts
            .iter()
            .map(|t| BatchEmbedRequest {
                model: &model_path,
                content: Content {
                    parts: vec![Part {
                        text: t.to_string(),
                    }],
                },
                output_dimensionality: Some(self.dimensions),
            })
            .collect();

        let body = BatchEmbedContentsRequest {
            requests: &requests,
        };

        let response = self
            .client
            .post(self.batch_url())
            .header("x-goog-api-key", &self.api_key)
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
                "Gemini batch API returned {status}: {text}"
            )));
        }

        let result: BatchEmbedContentsResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::Api(format!("Failed to parse Gemini response: {e}")))?;

        let embeddings: Vec<Vec<f32>> = result.embeddings.into_iter().map(|e| e.values).collect();

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

impl EmbeddingProvider for GeminiEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.request_embedding(text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.request_batch(texts).await
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

// -- Single embed request/response --

#[derive(Serialize)]
struct EmbedContentRequest {
    content: Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dimensionality: Option<usize>,
}

#[derive(Serialize)]
struct Content {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct Part {
    text: String,
}

#[derive(Deserialize)]
struct EmbedContentResponse {
    embedding: EmbeddingValues,
}

#[derive(Deserialize)]
struct EmbeddingValues {
    values: Vec<f32>,
}

// -- Batch embed request/response --

#[derive(Serialize)]
struct BatchEmbedContentsRequest<'a> {
    requests: &'a [BatchEmbedRequest<'a>],
}

#[derive(Serialize)]
struct BatchEmbedRequest<'a> {
    model: &'a str,
    content: Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dimensionality: Option<usize>,
}

#[derive(Deserialize)]
struct BatchEmbedContentsResponse {
    embeddings: Vec<EmbeddingValues>,
}
