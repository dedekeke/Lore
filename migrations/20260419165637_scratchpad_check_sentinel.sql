-- Guard against task_id holding the sentinel UUID used by
-- idx_scratchpad_scope_key's COALESCE fallback. A real task with that UUID
-- would conflict with the project-scoped NULL namespace on upsert.
ALTER TABLE ai_memory.scratchpad
    ADD CONSTRAINT scratchpad_task_id_not_sentinel
    CHECK (task_id IS NULL OR task_id <> '00000000-0000-0000-0000-000000000000'::uuid);
