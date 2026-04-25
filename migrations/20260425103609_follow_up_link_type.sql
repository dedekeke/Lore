-- Promote the analytics follow-up heuristic to an explicit link_type.
-- Prior heuristic in dashboard analytics_page used
--   `child.created_at > parent.completed_at`
-- which has two documented false-negative paths:
--   (a) subtask created between real work finishing and `complete_task`
--       being called
--   (b) bulk-imported subtasks with older `created_at` than parent.completed_at

-- The original migration declared the CHECK inline, so PG auto-named it
-- `task_links_link_type_check`. To stay correct under pg_dump/restore or
-- ad-hoc DDL that may have renamed it, drop ANY CHECK constraint on the
-- link_type column via the catalog, then re-add the constraint with an
-- explicit canonical name. Belt-and-suspenders against the silent two-
-- constraint state where the new ADD succeeds but the old one still rejects
-- 'follow_up'.
DO $$
DECLARE
    cname TEXT;
BEGIN
    FOR cname IN
        SELECT con.conname
        FROM pg_constraint con
        JOIN pg_class rel ON rel.oid = con.conrelid
        JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace
        JOIN pg_attribute att
          ON att.attrelid = rel.oid
         AND att.attnum = ANY (con.conkey)
        WHERE nsp.nspname = 'ai_memory'
          AND rel.relname = 'task_links'
          AND con.contype = 'c'
          AND att.attname = 'link_type'
    LOOP
        EXECUTE format('ALTER TABLE ai_memory.task_links DROP CONSTRAINT %I', cname);
    END LOOP;
END $$;

ALTER TABLE ai_memory.task_links
    ADD CONSTRAINT task_links_link_type_check
    CHECK (link_type IN ('blocks', 'related_to', 'caused_by', 'duplicate_of', 'follow_up'));

-- Backfill: every parent/child pair the heuristic currently fires on gets a
-- `follow_up` link. The existing UNIQUE on (source, target, link_type) keeps
-- this idempotent on re-run.
--
-- Historical undercount note: pairs that the heuristic missed (subtask
-- created before complete_task, or bulk-imported with older created_at)
-- are NOT backfilled here — only pairs satisfying the original heuristic
-- are preserved. New writes from `complete_task` will be exact going forward;
-- pre-migration analytics may show a lower follow_up count than reality.
INSERT INTO ai_memory.task_links (source_task_id, target_task_id, link_type)
SELECT parent.id, child.id, 'follow_up'
FROM ai_memory.tasks parent
JOIN ai_memory.tasks child ON child.parent_task_id = parent.id
WHERE parent.status = 'completed'
  AND parent.completed_at IS NOT NULL
  AND child.created_at > parent.completed_at
ON CONFLICT (source_task_id, target_task_id, link_type) DO NOTHING;
