mod common;

use lore::db::tasks::TaskStatus;
use lore::db::{projects, tasks};

#[tokio::test]
async fn test_create_and_get_task() {
    let (pool, _c) = common::setup_db().await;
    let pid = projects::create_project(&pool, "p", "/").await.unwrap();

    let tid = tasks::create_task(&pool, pid, "do something", None)
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

    let t1 = tasks::create_task(&pool, pid, "task1", None).await.unwrap();
    tasks::create_task(&pool, pid, "task2", None).await.unwrap();
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
    let tid = tasks::create_task(&pool, pid, "finish me", None)
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
    let tid = tasks::create_task(&pool, pid, "block me", None)
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

    let parent = tasks::create_task(&pool, pid, "parent", None)
        .await
        .unwrap();
    let child = tasks::create_task(&pool, pid, "child", Some(parent))
        .await
        .unwrap();

    let task = tasks::get_task(&pool, child).await.unwrap().unwrap();
    assert_eq!(task.parent_task_id, Some(parent));
}
