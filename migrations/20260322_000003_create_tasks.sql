CREATE TYPE ai_memory.task_status AS ENUM ('active', 'completed', 'abandoned', 'blocked');

CREATE TABLE ai_memory.tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES ai_memory.projects(id) ON DELETE CASCADE,
    description TEXT NOT NULL,
    status ai_memory.task_status NOT NULL DEFAULT 'active',
    parent_task_id UUID REFERENCES ai_memory.tasks(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX idx_tasks_project_status ON ai_memory.tasks(project_id, status);
