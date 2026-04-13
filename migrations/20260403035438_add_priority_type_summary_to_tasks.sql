ALTER TABLE ai_memory.tasks ADD COLUMN IF NOT EXISTS priority TEXT;
ALTER TABLE ai_memory.tasks ADD COLUMN IF NOT EXISTS task_type TEXT;
ALTER TABLE ai_memory.tasks ADD COLUMN IF NOT EXISTS summary TEXT;

CREATE INDEX IF NOT EXISTS idx_tasks_priority ON ai_memory.tasks(priority) WHERE priority IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tasks_type ON ai_memory.tasks(task_type) WHERE task_type IS NOT NULL;
