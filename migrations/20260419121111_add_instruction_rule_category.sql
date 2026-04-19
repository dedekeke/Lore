-- ALTER TYPE ADD VALUE commits in its own migration before any subsequent
-- migration references 'instruction'; do not merge this file with DML that
-- uses the new label — Postgres rejects the reference inside the same txn.
ALTER TYPE ai_memory.rule_category ADD VALUE IF NOT EXISTS 'instruction';
