ALTER TABLE ai_memory.code_chunks
    ADD COLUMN summary TEXT,
    ADD COLUMN behavior_version INTEGER NOT NULL DEFAULT 1;

CREATE INDEX idx_code_chunks_behavior_version
    ON ai_memory.code_chunks(project_id, behavior_version);
