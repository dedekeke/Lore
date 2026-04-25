-- External tracker reference (e.g. Jira "ABC-123", Linear "ENG-42").
-- Free-form text; surfaced in get_next_steps output and editable on dashboard.
ALTER TABLE ai_memory.tasks
    ADD COLUMN ticket_number TEXT NULL,
    ADD CONSTRAINT tasks_ticket_number_nonempty
        CHECK (ticket_number IS NULL OR length(btrim(ticket_number)) > 0);

CREATE INDEX idx_tasks_ticket_number
    ON ai_memory.tasks (ticket_number)
    WHERE ticket_number IS NOT NULL;
