-- Per-project URL template for rendering ticket_number badges as clickable
-- links on the dashboard. Must contain the literal placeholder "{ticket}",
-- which the renderer substitutes (URL-encoded) per task.
ALTER TABLE ai_memory.projects
    ADD COLUMN ticket_url_template TEXT NULL,
    ADD CONSTRAINT projects_ticket_url_template_has_placeholder
        CHECK (ticket_url_template IS NULL OR ticket_url_template LIKE '%{ticket}%');
