ALTER TABLE ai_memory.semantic_rules ADD COLUMN IF NOT EXISTS tags TEXT[] NOT NULL DEFAULT '{}';
CREATE INDEX IF NOT EXISTS idx_semantic_rules_tags ON ai_memory.semantic_rules USING GIN (tags);
