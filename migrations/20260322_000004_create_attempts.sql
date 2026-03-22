CREATE TYPE ai_memory.attempt_outcome AS ENUM ('pending', 'accepted', 'rejected');

CREATE TABLE ai_memory.attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES ai_memory.tasks(id) ON DELETE CASCADE,
    approach_summary TEXT NOT NULL,
    code_snippet TEXT,
    outcome ai_memory.attempt_outcome NOT NULL DEFAULT 'pending',
    reasoning TEXT NOT NULL DEFAULT '',
    reasoning_embedding vector(384),
    git_ref TEXT,
    token_cost INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);

CREATE INDEX idx_attempts_task ON ai_memory.attempts(task_id);
CREATE INDEX idx_attempts_outcome ON ai_memory.attempts(task_id, outcome);
CREATE INDEX idx_attempts_reasoning_embedding ON ai_memory.attempts
    USING hnsw (reasoning_embedding vector_cosine_ops)
    WHERE reasoning_embedding IS NOT NULL;

-- Add FK from semantic_rules to tasks now that tasks table exists
ALTER TABLE ai_memory.semantic_rules
    ADD CONSTRAINT fk_semantic_source_task
    FOREIGN KEY (source_task_id) REFERENCES ai_memory.tasks(id) ON DELETE SET NULL;

-- Index for FK lookups on source_task_id (needed for ON DELETE SET NULL scans)
CREATE INDEX idx_semantic_source_task ON ai_memory.semantic_rules(source_task_id);
