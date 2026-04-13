ALTER TABLE ai_memory.semantic_rules ADD COLUMN tags TEXT[] NOT NULL DEFAULT '{}';
CREATE INDEX idx_semantic_rules_tags ON ai_memory.semantic_rules USING GIN (tags);
