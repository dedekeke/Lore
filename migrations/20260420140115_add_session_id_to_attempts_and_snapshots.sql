ALTER TABLE ai_memory.attempts ADD COLUMN session_id TEXT;
ALTER TABLE ai_memory.attempts ADD COLUMN resolved_by_agent_id TEXT;
ALTER TABLE ai_memory.attempts ADD COLUMN resolved_by_session_id TEXT;

ALTER TABLE ai_memory.context_snapshots ADD COLUMN agent_id TEXT;
ALTER TABLE ai_memory.context_snapshots ADD COLUMN session_id TEXT;
