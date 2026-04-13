ALTER TABLE ai_memory.code_chunks
    ADD COLUMN IF NOT EXISTS summary TEXT,
    ADD COLUMN IF NOT EXISTS behavior_version INTEGER NOT NULL DEFAULT 1;

CREATE INDEX IF NOT EXISTS idx_code_chunks_behavior_version
    ON ai_memory.code_chunks(project_id, behavior_version);
