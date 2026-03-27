pub mod fake;
pub mod gemini;
#[cfg(feature = "local-embeddings")]
pub mod local;

use crate::config::{self, Config};

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("Embedding model error: {0}")]
    Model(String),

    #[error("API request failed: {0}")]
    Api(String),

    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
}

/// Trait for generating text embeddings.
/// Uses native async fn in traits (Rust 1.75+) — not dyn-compatible.
pub trait EmbeddingProvider: Send + Sync {
    fn embed(
        &self,
        text: &str,
    ) -> impl std::future::Future<Output = Result<Vec<f32>, EmbeddingError>> + Send;

    fn embed_batch(
        &self,
        texts: &[&str],
    ) -> impl std::future::Future<Output = Result<Vec<Vec<f32>>, EmbeddingError>> + Send;

    fn dimensions(&self) -> usize;
}

/// Enum dispatch for runtime provider selection (avoids dyn + async_trait).
#[allow(clippy::large_enum_variant)]
pub enum AnyEmbeddingProvider {
    #[cfg(feature = "local-embeddings")]
    Local(local::LocalEmbeddingProvider),
    Gemini(gemini::GeminiEmbeddingProvider),
    Fake(fake::FakeEmbeddingProvider),
}

impl EmbeddingProvider for AnyEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        match self {
            #[cfg(feature = "local-embeddings")]
            Self::Local(p) => p.embed(text).await,
            Self::Gemini(p) => p.embed(text).await,
            Self::Fake(p) => p.embed(text).await,
        }
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        match self {
            #[cfg(feature = "local-embeddings")]
            Self::Local(p) => p.embed_batch(texts).await,
            Self::Gemini(p) => p.embed_batch(texts).await,
            Self::Fake(p) => p.embed_batch(texts).await,
        }
    }

    fn dimensions(&self) -> usize {
        match self {
            #[cfg(feature = "local-embeddings")]
            Self::Local(p) => p.dimensions(),
            Self::Gemini(p) => p.dimensions(),
            Self::Fake(p) => p.dimensions(),
        }
    }
}

pub fn create_provider(config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    match config.embedding_provider {
        config::EmbeddingProvider::Local => create_local_provider(config),
        config::EmbeddingProvider::Gemini => create_gemini_provider(config),
    }
}

#[cfg(feature = "local-embeddings")]
fn create_local_provider(config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    let provider =
        local::LocalEmbeddingProvider::new(&config.embedding_model, config.embedding_dimensions)?;
    Ok(AnyEmbeddingProvider::Local(provider))
}

#[cfg(not(feature = "local-embeddings"))]
fn create_local_provider(_config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    Err(EmbeddingError::Model(
        "Local embedding provider requires the 'local-embeddings' feature flag. \
         Recompile with `--features local-embeddings` or switch to the Gemini provider."
            .to_string(),
    ))
}

fn create_gemini_provider(config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    let api_key = config
        .gemini_api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            EmbeddingError::Api(
                "GEMINI_API_KEY is required when using the Gemini embedding provider".to_string(),
            )
        })?;

    tracing::info!(
        model = %config.embedding_model,
        dimensions = config.embedding_dimensions,
        "Creating Gemini embedding provider"
    );
    let provider = gemini::GeminiEmbeddingProvider::new(
        api_key,
        &config.embedding_model,
        config.embedding_dimensions,
    );
    Ok(AnyEmbeddingProvider::Gemini(provider))
}
