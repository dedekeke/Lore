ALTER TABLE ai_memory.semantic_rules
    ADD COLUMN valid_from TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ADD COLUMN valid_until TIMESTAMPTZ;

-- Backfill: existing rules get valid_from = created_at
UPDATE ai_memory.semantic_rules SET valid_from = created_at;

CREATE INDEX idx_semantic_rules_active ON ai_memory.semantic_rules (project_id)
    WHERE valid_until IS NULL;
