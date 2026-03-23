use super::{EmbeddingError, EmbeddingProvider};

pub struct FakeEmbeddingProvider {
    dims: usize,
}

impl FakeEmbeddingProvider {
    pub fn new(dims: usize) -> Self {
        Self { dims }
    }
}

impl EmbeddingProvider for FakeEmbeddingProvider {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(vec![0.1; self.dims])
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|_| vec![0.1; self.dims]).collect())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}
