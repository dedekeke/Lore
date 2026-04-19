ALTER TABLE ai_memory.semantic_rules
    ADD COLUMN is_always_injected BOOLEAN NOT NULL DEFAULT false;

CREATE INDEX idx_semantic_always_injected
    ON ai_memory.semantic_rules(project_id)
    WHERE is_always_injected = true AND valid_until IS NULL;
