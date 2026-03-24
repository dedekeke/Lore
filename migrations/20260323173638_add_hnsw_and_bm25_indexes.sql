-- HNSW indexes for approximate nearest neighbor search (O(log N) vs O(N))
CREATE INDEX IF NOT EXISTS idx_semantic_embedding_hnsw
    ON ai_memory.semantic_rules
    USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);

CREATE INDEX IF NOT EXISTS idx_attempts_reasoning_embedding_hnsw
    ON ai_memory.attempts
    USING hnsw (reasoning_embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);

-- Full-text search columns + GIN indexes for BM25-style keyword search
ALTER TABLE ai_memory.semantic_rules
    ADD COLUMN IF NOT EXISTS content_tsv tsvector
    GENERATED ALWAYS AS (to_tsvector('english', content)) STORED;

ALTER TABLE ai_memory.attempts
    ADD COLUMN IF NOT EXISTS reasoning_tsv tsvector
    GENERATED ALWAYS AS (to_tsvector('english', reasoning)) STORED;

CREATE INDEX IF NOT EXISTS idx_semantic_content_fts
    ON ai_memory.semantic_rules USING gin (content_tsv);

CREATE INDEX IF NOT EXISTS idx_attempts_reasoning_fts
    ON ai_memory.attempts USING gin (reasoning_tsv);
