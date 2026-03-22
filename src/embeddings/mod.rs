//! Embedding provider trait and factory.
//!
//! Defines the [`EmbeddingProvider`] trait that all embedding backends must
//! implement. Because native async fn in traits returns `impl Future` which
//! is not dyn-compatible, the factory uses an [`AnyEmbeddingProvider`] enum
//! to dispatch at runtime without requiring the `async_trait` crate.

pub mod local;
pub mod openai;

use crate::config::{self, Config};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during embedding generation.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("Embedding model error: {0}")]
    Model(String),

    #[error("API request failed: {0}")]
    Api(String),

    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Trait for generating text embeddings.
///
/// Implementations must be safe to share across threads (`Send + Sync`).
/// Uses native async fn in traits (Rust 1.75+).
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding vector for the given text.
    fn embed(
        &self,
        text: &str,
    ) -> impl std::future::Future<Output = Result<Vec<f32>, EmbeddingError>> + Send;

    /// Generate embeddings for multiple texts in a batch.
    fn embed_batch(
        &self,
        texts: &[&str],
    ) -> impl std::future::Future<Output = Result<Vec<Vec<f32>>, EmbeddingError>> + Send;

    /// Return the dimensionality of embeddings produced by this provider.
    fn dimensions(&self) -> usize;
}

// ---------------------------------------------------------------------------
// Enum dispatch (avoids dyn + async_trait)
// ---------------------------------------------------------------------------

/// Runtime-dispatched embedding provider.
///
/// Since native async fn in traits is not dyn-compatible, this enum provides
/// concrete dispatch without needing `async_trait` or `Box<dyn ...>`.
pub enum AnyEmbeddingProvider {
    #[cfg(feature = "local-embeddings")]
    Local(local::LocalEmbeddingProvider),
    OpenAi(openai::OpenAiEmbeddingProvider),
}

impl EmbeddingProvider for AnyEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        match self {
            #[cfg(feature = "local-embeddings")]
            Self::Local(p) => p.embed(text).await,
            Self::OpenAi(p) => p.embed(text).await,
        }
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        match self {
            #[cfg(feature = "local-embeddings")]
            Self::Local(p) => p.embed_batch(texts).await,
            Self::OpenAi(p) => p.embed_batch(texts).await,
        }
    }

    fn dimensions(&self) -> usize {
        match self {
            #[cfg(feature = "local-embeddings")]
            Self::Local(p) => p.dimensions(),
            Self::OpenAi(p) => p.dimensions(),
        }
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create the appropriate embedding provider based on application config.
///
/// - [`config::EmbeddingProvider::Local`] requires the `local-embeddings`
///   feature flag to be enabled at compile time.
/// - [`config::EmbeddingProvider::OpenAi`] requires `openai_api_key` to be
///   set in the config.
pub fn create_provider(config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    match config.embedding_provider {
        config::EmbeddingProvider::Local => create_local_provider(config),
        config::EmbeddingProvider::OpenAi => create_openai_provider(config),
    }
}

#[cfg(feature = "local-embeddings")]
fn create_local_provider(config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    let provider = local::LocalEmbeddingProvider::new(
        &config.embedding_model,
        config.embedding_dimensions,
    )?;
    Ok(AnyEmbeddingProvider::Local(provider))
}

#[cfg(not(feature = "local-embeddings"))]
fn create_local_provider(_config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    Err(EmbeddingError::Model(
        "Local embedding provider requires the 'local-embeddings' feature flag. \
         Recompile with `--features local-embeddings` or switch to the OpenAI provider."
            .to_string(),
    ))
}

fn create_openai_provider(config: &Config) -> Result<AnyEmbeddingProvider, EmbeddingError> {
    let api_key = config
        .openai_api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            EmbeddingError::Api(
                "OPENAI_API_KEY is required when using the OpenAI embedding provider".to_string(),
            )
        })?;

    let provider = openai::OpenAiEmbeddingProvider::new(
        api_key,
        &config.embedding_model,
        config.embedding_dimensions,
    );
    Ok(AnyEmbeddingProvider::OpenAi(provider))
}
