CREATE TABLE ai_memory.context_snapshots (
                                             id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                                             task_id UUID NOT NULL REFERENCES ai_memory.tasks(id) ON DELETE CASCADE,
                                             wiped_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                                             token_count_before INTEGER NOT NULL,
                                             last_attempt_id UUID REFERENCES ai_memory.attempts(id) ON DELETE SET NULL
);

CREATE INDEX idx_snapshots_task ON ai_memory.context_snapshots(task_id);
