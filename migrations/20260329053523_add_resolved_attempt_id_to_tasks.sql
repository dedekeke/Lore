ALTER TABLE ai_memory.tasks
ADD COLUMN resolved_attempt_id UUID REFERENCES ai_memory.attempts(id);
