-- Add 'unknown' variant to attempt_outcome enum for stale pending attempts
ALTER TYPE ai_memory.attempt_outcome ADD VALUE IF NOT EXISTS 'unknown' AFTER 'rejected';
