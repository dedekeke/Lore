ALTER TABLE ai_memory.tasks ADD COLUMN priority TEXT;
ALTER TABLE ai_memory.tasks ADD COLUMN task_type TEXT;
ALTER TABLE ai_memory.tasks ADD COLUMN summary TEXT;

CREATE INDEX idx_tasks_priority ON ai_memory.tasks(priority) WHERE priority IS NOT NULL;
CREATE INDEX idx_tasks_type ON ai_memory.tasks(task_type) WHERE task_type IS NOT NULL;
