CREATE TYPE ai_memory.rule_category AS ENUM ('preference', 'fact', 'constraint', 'lesson');

CREATE TABLE ai_memory.semantic_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES ai_memory.projects(id) ON DELETE CASCADE,
    category ai_memory.rule_category NOT NULL,
    content TEXT NOT NULL,
    embedding vector(384),
    source_task_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);

CREATE INDEX idx_semantic_project ON ai_memory.semantic_rules(project_id);
CREATE INDEX idx_semantic_category ON ai_memory.semantic_rules(project_id, category);
CREATE INDEX idx_semantic_embedding ON ai_memory.semantic_rules
    USING hnsw (embedding vector_cosine_ops)
    WHERE embedding IS NOT NULL;
