mod common;

use lore::db::attempts::{self, AttemptOutcome};
use lore::db::tasks::TaskStatus;
use lore::db::{projects, tasks};

#[tokio::test]
async fn test_create_and_get_task() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let tid = tasks::create_task(&pool, pid, "do something", None, None, None, None)
        .await
        .unwrap();
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();

    assert_eq!(task.description, "do something");
    assert_eq!(task.status, TaskStatus::Active);
    assert!(task.completed_at.is_none());
}

#[tokio::test]
async fn test_list_tasks_with_status_filter() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "task1", None, None, None, None)
        .await
        .unwrap();
    tasks::create_task(&pool, pid, "task2", None, None, None, None)
        .await
        .unwrap();
    tasks::complete_task(&pool, t1, None).await.unwrap();

    let active = tasks::list_tasks(&pool, pid, Some(TaskStatus::Active))
        .await
        .unwrap();
    let all = tasks::list_tasks(&pool, pid, None).await.unwrap();

    assert_eq!(active.len(), 1);
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_complete_task() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "finish me", None, None, None, None)
        .await
        .unwrap();

    assert!(tasks::complete_task(&pool, tid, None).await.unwrap());

    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(task.completed_at.is_some());
}

#[tokio::test]
async fn test_update_task_status() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();
    let tid = tasks::create_task(&pool, pid, "block me", None, None, None, None)
        .await
        .unwrap();

    tasks::update_task_status(&pool, tid, TaskStatus::Blocked)
        .await
        .unwrap();
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Blocked);
}

#[tokio::test]
async fn test_subtask_parent() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let parent = tasks::create_task(&pool, pid, "parent", None, None, None, None)
        .await
        .unwrap();
    let child = tasks::create_task(&pool, pid, "child", Some(parent), None, None, None)
        .await
        .unwrap();

    let task = tasks::get_task(&pool, child).await.unwrap().unwrap();
    assert_eq!(task.parent_task_id, Some(parent));
}

#[tokio::test]
async fn test_get_task_stats_no_tasks() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let stats = tasks::get_task_stats(&pool, pid, None).await.unwrap();
    assert!(stats.is_empty());
}

#[tokio::test]
async fn test_get_task_stats_with_attempts() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "stats task", None, None, None, None)
        .await
        .unwrap();
    let a1 = attempts::create_attempt(&pool, t1, "approach 1", None, None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        a1,
        AttemptOutcome::Rejected,
        "bad approach",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let a2 = attempts::create_attempt(&pool, t1, "approach 2", None, None, None)
        .await
        .unwrap();
    attempts::log_outcome(
        &pool,
        a2,
        AttemptOutcome::Accepted,
        "works",
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // Active task — no resolution_minutes
    let stats = tasks::get_task_stats(&pool, pid, None).await.unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].total_attempts, 2);
    assert_eq!(stats[0].rejected_attempts, 1);
    assert_eq!(stats[0].accepted_attempts, 1);
    assert!(stats[0].resolution_minutes.is_none());

    // Complete the task — now resolution_minutes should be set
    tasks::complete_task(&pool, t1, Some(a2)).await.unwrap();
    let stats = tasks::get_task_stats(&pool, pid, None).await.unwrap();
    assert!(stats[0].resolution_minutes.is_some());
}

#[tokio::test]
async fn test_get_task_stats_filtered_by_status() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let t1 = tasks::create_task(&pool, pid, "active", None, None, None, None)
        .await
        .unwrap();
    let t2 = tasks::create_task(&pool, pid, "done", None, None, None, None)
        .await
        .unwrap();
    tasks::complete_task(&pool, t2, None).await.unwrap();

    let active = tasks::get_task_stats(&pool, pid, Some(TaskStatus::Active))
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, t1);

    let completed = tasks::get_task_stats(&pool, pid, Some(TaskStatus::Completed))
        .await
        .unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].id, t2);
}

#[tokio::test]
async fn test_create_task_with_description_embedding() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.42_f32; 384];
    let tid = tasks::create_task(&pool, pid, "embedded task", None, None, None, Some(&emb))
        .await
        .unwrap();

    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert_eq!(task.description, "embedded task");
    let stored = task.description_embedding.unwrap();
    let stored_vec: Vec<f32> = stored.to_vec();
    assert_eq!(stored_vec.len(), 384);
    assert!((stored_vec[0] - 0.42).abs() < 1e-6);
}

#[tokio::test]
async fn test_create_task_without_embedding() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let tid = tasks::create_task(&pool, pid, "no emb", None, None, None, None)
        .await
        .unwrap();
    let task = tasks::get_task(&pool, tid).await.unwrap().unwrap();
    assert!(task.description_embedding.is_none());
}

#[tokio::test]
async fn test_list_tasks_includes_embedding() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let emb = vec![0.1_f32; 384];
    tasks::create_task(&pool, pid, "t1", None, None, None, Some(&emb))
        .await
        .unwrap();
    tasks::create_task(&pool, pid, "t2", None, None, None, None)
        .await
        .unwrap();

    let all = tasks::list_tasks(&pool, pid, None).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|t| t.description_embedding.is_some()));
    assert!(all.iter().any(|t| t.description_embedding.is_none()));
}
