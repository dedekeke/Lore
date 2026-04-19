ALTER TABLE ai_memory.code_chunks ADD COLUMN IF NOT EXISTS community_id INTEGER;

CREATE INDEX IF NOT EXISTS idx_code_chunks_community
    ON ai_memory.code_chunks(project_id, community_id)
    WHERE community_id IS NOT NULL;
