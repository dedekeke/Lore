CREATE TABLE IF NOT EXISTS ai_memory.codebase_edges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES ai_memory.projects(id) ON DELETE CASCADE,
    source_entity TEXT NOT NULL,
    target_entity TEXT NOT NULL,
    edge_type TEXT NOT NULL CHECK (edge_type IN ('calls', 'uses', 'contains')),
    source_file TEXT,
    target_file TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_codebase_edges_source ON ai_memory.codebase_edges(project_id, source_entity);
CREATE INDEX idx_codebase_edges_target ON ai_memory.codebase_edges(project_id, target_entity);
CREATE UNIQUE INDEX idx_codebase_edges_unique ON ai_memory.codebase_edges(project_id, source_entity, target_entity, edge_type);
