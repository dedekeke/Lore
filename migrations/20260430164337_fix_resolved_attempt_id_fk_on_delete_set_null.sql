ALTER TABLE ai_memory.tasks
    DROP CONSTRAINT IF EXISTS tasks_resolved_attempt_id_fkey,
    ADD CONSTRAINT tasks_resolved_attempt_id_fkey
        FOREIGN KEY (resolved_attempt_id)
        REFERENCES ai_memory.attempts(id)
        ON DELETE SET NULL;
