ALTER TABLE ai_memory.attempts ADD COLUMN IF NOT EXISTS session_id TEXT;
ALTER TABLE ai_memory.attempts ADD COLUMN IF NOT EXISTS resolved_by_agent_id TEXT;
ALTER TABLE ai_memory.attempts ADD COLUMN IF NOT EXISTS resolved_by_session_id TEXT;

ALTER TABLE ai_memory.context_snapshots ADD COLUMN IF NOT EXISTS agent_id TEXT;
ALTER TABLE ai_memory.context_snapshots ADD COLUMN IF NOT EXISTS session_id TEXT;
