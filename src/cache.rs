use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;

/// Cached embedding result keyed by input text hash
type EmbeddingCache = Cache<u64, Arc<Vec<f32>>>;

/// Cached search results keyed by (query_hash, project_id)
type SearchCache = Cache<u64, Arc<String>>;

pub struct LoreCache {
    pub embeddings: EmbeddingCache,
    pub search: SearchCache,
}

impl LoreCache {
    pub fn new(embedding_capacity: u64, search_capacity: u64) -> Self {
        Self {
            embeddings: Cache::builder()
                .max_capacity(embedding_capacity)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            search: Cache::builder()
                .max_capacity(search_capacity)
                .time_to_live(Duration::from_secs(60))
                .build(),
        }
    }
}

/// Fast hash for cache keys — not cryptographic, just for dedup
pub fn hash_key(data: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}
