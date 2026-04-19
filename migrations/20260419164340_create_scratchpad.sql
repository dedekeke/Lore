CREATE TABLE ai_memory.scratchpad (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES ai_memory.projects(id) ON DELETE CASCADE,
    task_id UUID REFERENCES ai_memory.tasks(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Upsert scope: one value per (project, task, key). NULL task_id groups
-- under a per-project scratch namespace distinct from any task.
CREATE UNIQUE INDEX idx_scratchpad_scope_key
    ON ai_memory.scratchpad(project_id, COALESCE(task_id, '00000000-0000-0000-0000-000000000000'::uuid), key);

CREATE INDEX idx_scratchpad_project_task
    ON ai_memory.scratchpad(project_id, task_id);

CREATE INDEX idx_scratchpad_expires
    ON ai_memory.scratchpad(expires_at)
    WHERE expires_at IS NOT NULL;
