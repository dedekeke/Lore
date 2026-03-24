use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;

type EmbeddingCache = Cache<String, Arc<Vec<f32>>>;
type SearchCache = Cache<String, Arc<String>>;

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

    pub fn invalidate_search(&self) {
        self.search.invalidate_all();
    }
}
