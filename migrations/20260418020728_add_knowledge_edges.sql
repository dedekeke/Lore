CREATE TABLE IF NOT EXISTS ai_memory.knowledge_edges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_entity TEXT NOT NULL,
    target_entity TEXT NOT NULL,
    edge_type TEXT NOT NULL,
    confidence FLOAT DEFAULT 1.0,
    source_task_id UUID REFERENCES ai_memory.tasks(id) ON DELETE SET NULL,
    project_id UUID NOT NULL REFERENCES ai_memory.projects(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(source_entity, target_entity, edge_type, project_id)
);
CREATE INDEX idx_knowledge_edges_source ON ai_memory.knowledge_edges(source_entity, project_id);
CREATE INDEX idx_knowledge_edges_target ON ai_memory.knowledge_edges(target_entity, project_id);
