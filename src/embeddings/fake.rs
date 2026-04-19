use sha2::{Digest, Sha256};

use super::{EmbeddingError, EmbeddingProvider};

pub struct FakeEmbeddingProvider {
    dims: usize,
    hashed: bool,
}

impl FakeEmbeddingProvider {
    /// Constant-vector provider (every text → `[0.1; dims]`). Used by the
    /// older integration tests that insert at most one rule per project.
    pub fn new(dims: usize) -> Self {
        Self {
            dims,
            hashed: false,
        }
    }

    /// Deterministic hash-seeded provider: distinct texts produce distinct,
    /// L2-normalised vectors. Used by the eval harness so multiple rules in
    /// one project don't collapse under the 0.95 cosine dedup threshold.
    pub fn hashed(dims: usize) -> Self {
        Self { dims, hashed: true }
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        if !self.hashed {
            return vec![0.1; self.dims];
        }
        let mut out = Vec::with_capacity(self.dims);
        let mut counter: u32 = 0;
        while out.len() < self.dims {
            let mut h = Sha256::new();
            h.update(text.as_bytes());
            h.update(counter.to_le_bytes());
            for chunk in h.finalize().chunks_exact(4) {
                if out.len() >= self.dims {
                    break;
                }
                let u = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                out.push((u as f32 / u32::MAX as f32) * 2.0 - 1.0);
            }
            counter = counter.wrapping_add(1);
        }
        let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut out {
                *x /= norm;
            }
        }
        out
    }
}

impl EmbeddingProvider for FakeEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        Ok(self.embed_text(text))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|t| self.embed_text(t)).collect())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashed_is_distinct_per_text() {
        // Use production dim (384) so the dedup-threshold guarantee is
        // validated at the same shape the eval harness actually runs with.
        let p = FakeEmbeddingProvider::hashed(384);
        let a = p.embed("foo").await.unwrap();
        let b = p.embed("bar").await.unwrap();
        assert_ne!(a, b);
        let dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!(
            dot.abs() < 0.95,
            "cosine between distinct texts too high: {dot}"
        );
    }

    #[tokio::test]
    async fn hashed_is_stable_across_calls() {
        let p = FakeEmbeddingProvider::hashed(32);
        assert_eq!(p.embed("x").await.unwrap(), p.embed("x").await.unwrap());
    }
}
