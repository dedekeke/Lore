ALTER TABLE ai_memory.semantic_rules ADD COLUMN IF NOT EXISTS hit_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ai_memory.semantic_rules ADD COLUMN IF NOT EXISTS last_used_at TIMESTAMPTZ;
ALTER TABLE ai_memory.semantic_rules ADD COLUMN IF NOT EXISTS weight FLOAT;
ALTER TABLE ai_memory.semantic_rules ADD COLUMN IF NOT EXISTS task_type_affinity TEXT[];

CREATE INDEX IF NOT EXISTS idx_semantic_hit_count ON ai_memory.semantic_rules(project_id, hit_count DESC);
