mod common;

use lore::db::{attempts, projects, retention, tasks};

#[tokio::test]
async fn test_prune_old_attempts() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "task", None).await.unwrap();
    let aid = attempts::create_attempt(&pool, tid, "old attempt", None)
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
    let tid = tasks::create_task(&pool, pid, "old task", None)
        .await
        .unwrap();

    tasks::complete_task(&pool, tid).await.unwrap();

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
    let tid = tasks::create_task(&pool, pid, "task", None).await.unwrap();

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
