-- Promote the analytics follow-up heuristic to an explicit link_type.
-- Prior heuristic in dashboard analytics_page used
--   `child.created_at > parent.completed_at`
-- which has two documented false-negative paths:
--   (a) subtask created between real work finishing and `complete_task`
--       being called
--   (b) bulk-imported subtasks with older `created_at` than parent.completed_at

ALTER TABLE ai_memory.task_links
    DROP CONSTRAINT IF EXISTS task_links_link_type_check;

ALTER TABLE ai_memory.task_links
    ADD CONSTRAINT task_links_link_type_check
    CHECK (link_type IN ('blocks', 'related_to', 'caused_by', 'duplicate_of', 'follow_up'));

-- Backfill: every parent/child pair the heuristic currently fires on gets a
-- `follow_up` link. The existing UNIQUE on (source, target, link_type) keeps
-- this idempotent on re-run.
INSERT INTO ai_memory.task_links (source_task_id, target_task_id, link_type)
SELECT parent.id, child.id, 'follow_up'
FROM ai_memory.tasks parent
JOIN ai_memory.tasks child ON child.parent_task_id = parent.id
WHERE parent.status = 'completed'
  AND parent.completed_at IS NOT NULL
  AND child.created_at > parent.completed_at
ON CONFLICT (source_task_id, target_task_id, link_type) DO NOTHING;
