mod common;

use lore::db::semantic::RuleCategory;
use lore::db::{attempts, projects, retention, semantic, tasks, AttemptOutcome};

#[tokio::test]
async fn test_prune_old_attempts() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "task", None, None, None, None)
        .await
        .unwrap();
    let aid = attempts::create_attempt(&pool, tid, "old attempt", None, None)
        .await
        .unwrap();

    // Backdate the attempt
    sqlx::query(
        "UPDATE ai_memory.attempts SET created_at = NOW() - interval '100 days' WHERE id = $1",
    )
    .bind(aid)
    .execute(&pool)
    .await
    .unwrap();

    let pruned = retention::prune_old_attempts(&pool, 30).await.unwrap();
    assert_eq!(pruned, 1);

    assert!(attempts::get_attempt(&pool, aid).await.unwrap().is_none());
}

#[tokio::test]
async fn test_purge_completed_tasks() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "old task", None, None, None, None)
        .await
        .unwrap();

    tasks::complete_task(&pool, tid, None).await.unwrap();

    // Backdate completed_at
    sqlx::query(
        "UPDATE ai_memory.tasks SET completed_at = NOW() - interval '200 days' WHERE id = $1",
    )
    .bind(tid)
    .execute(&pool)
    .await
    .unwrap();

    let purged = retention::purge_completed_tasks(&pool, 90).await.unwrap();
    assert_eq!(purged, 1);

    assert!(tasks::get_task(&pool, tid).await.unwrap().is_none());
}

#[tokio::test]
async fn test_prune_old_snapshots() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "task", None, None, None, None)
        .await
        .unwrap();

    // Insert a snapshot directly
    sqlx::query(
        "INSERT INTO ai_memory.context_snapshots (task_id, token_count_before) VALUES ($1, 1000)",
    )
    .bind(tid)
    .execute(&pool)
    .await
    .unwrap();

    // Backdate it
    sqlx::query(
        "UPDATE ai_memory.context_snapshots SET wiped_at = NOW() - interval '30 days' WHERE task_id = $1"
    )
    .bind(tid)
    .execute(&pool)
    .await
    .unwrap();

    let pruned = retention::prune_old_snapshots(&pool, 7).await.unwrap();
    assert_eq!(pruned, 1);
}

#[tokio::test]
async fn test_consolidate_preserves_failure_narrative() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "build auth middleware", None, None, None, None)
        .await
        .unwrap();

    let rejected_id = attempts::create_attempt(&pool, tid, "use JWT in cookies", None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        rejected_id,
        AttemptOutcome::Rejected,
        "CSRF risk",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let accepted_id =
        attempts::create_attempt(&pool, tid, "use JWT in Authorization header", None, None)
            .await
            .unwrap();
    attempts::log_outcome(
        &pool,
        accepted_id,
        AttemptOutcome::Accepted,
        "stateless, CSRF-safe",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    tasks::complete_task(&pool, tid, None).await.unwrap();

    // Backdate both attempts past consolidation threshold
    sqlx::query("UPDATE ai_memory.attempts SET resolved_at = NOW() - interval '100 days'")
        .execute(&pool)
        .await
        .unwrap();

    let created = retention::consolidate_old_attempts(&pool, 30, 1)
        .await
        .unwrap();
    assert_eq!(created, 1);

    // Lesson has structured narrative
    let lessons = semantic::list_rules(&pool, pid, Some(RuleCategory::Lesson), None)
        .await
        .unwrap();
    assert_eq!(lessons.len(), 1);
    let content = &lessons[0].content;
    assert!(content.contains("## Task: build auth middleware"));
    assert!(content.contains("### Rejected approaches:"));
    assert!(content.contains("use JWT in cookies: CSRF risk"));
    assert!(content.contains("### Accepted approach:"));
    assert!(content.contains("use JWT in Authorization header: stateless, CSRF-safe"));

    // Both outcome types deleted
    assert!(attempts::get_attempt(&pool, rejected_id)
        .await
        .unwrap()
        .is_none());
    assert!(attempts::get_attempt(&pool, accepted_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_consolidate_skips_tasks_below_threshold() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "single-try task", None, None, None, None)
        .await
        .unwrap();

    let aid = attempts::create_attempt(&pool, tid, "approach", None, None)
        .await
        .unwrap();
    attempts::log_outcome(&pool, aid, AttemptOutcome::Accepted, "ok", None, None, None)
        .await
        .unwrap();
    tasks::complete_task(&pool, tid, None).await.unwrap();

    sqlx::query("UPDATE ai_memory.attempts SET resolved_at = NOW() - interval '100 days'")
        .execute(&pool)
        .await
        .unwrap();

    // min_accepted = 2, only 1 accepted → not eligible
    let created = retention::consolidate_old_attempts(&pool, 30, 2)
        .await
        .unwrap();
    assert_eq!(created, 0);
    assert!(attempts::get_attempt(&pool, aid).await.unwrap().is_some());
    let lessons = semantic::list_rules(&pool, pid, Some(RuleCategory::Lesson), None)
        .await
        .unwrap();
    assert!(lessons.is_empty());
}
