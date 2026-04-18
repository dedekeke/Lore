CREATE TABLE ai_memory.task_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_task_id UUID NOT NULL REFERENCES ai_memory.tasks(id) ON DELETE CASCADE,
    target_task_id UUID NOT NULL REFERENCES ai_memory.tasks(id) ON DELETE CASCADE,
    link_type TEXT NOT NULL CHECK (link_type IN ('blocks', 'related_to', 'caused_by', 'duplicate_of')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (source_task_id, target_task_id, link_type)
);

CREATE INDEX idx_task_links_source ON ai_memory.task_links (source_task_id);
CREATE INDEX idx_task_links_target ON ai_memory.task_links (target_task_id);
