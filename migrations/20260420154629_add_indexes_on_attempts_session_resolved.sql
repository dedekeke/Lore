-- Partial indexes on attempts.session_id + attempts.resolved_by_agent_id + attempts.resolved_by_session_id.
-- Rationale: dashboard queries filter "attempts for session X" or "attempts resolved by agent Y".
-- Partial on IS NOT NULL because resolver fields are only populated on finalized attempts
-- (log_outcome call site) — full index would store mostly NULLs.
-- IF NOT EXISTS for re-run safety.

CREATE INDEX IF NOT EXISTS idx_attempts_session_id
    ON ai_memory.attempts (session_id)
    WHERE session_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_attempts_resolved_by_agent_id
    ON ai_memory.attempts (resolved_by_agent_id)
    WHERE resolved_by_agent_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_attempts_resolved_by_session_id
    ON ai_memory.attempts (resolved_by_session_id)
    WHERE resolved_by_session_id IS NOT NULL;
